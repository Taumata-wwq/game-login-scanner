//! Windows Media Foundation H.264 解码（硬解 MFT）封装。
//!
//! 背景与选型（2026-10-11 调研结论，详见 docs/性能优化调研报告-2026-10-11.md）：
//! - 现用 openh264 为**单线程软解**（Windows 上 num_threads 必崩，无法多线程），
//!   1080p60 单线程 19~24ms/帧跟不上流帧间隔，导致追帧/降清晰度常态化；
//! - 系统自带的 H.264 解码 MFT（CLSID_CMSH264DecoderMFT）**自动优先走 DXVA 硬解、
//!   无硬件时软解回退**（微软签名组件，无需自行分支）；解码负载从 CPU 卸载到
//!   GPU 的专用解码单元，典型 CPU 占用从「占满一核」降至个位数百分比；
//! - 输入要求为 **Annex-B**（本管线已有 `avcc_to_annexb`，天然契合）；
//! - 输出 **NV12 系统内存**（copy-back 模式）：`decode()` 内完成 NV12→紧凑 I420
//!   转换，下游帧总线无需感知硬件路径。
//!
//! 可用性：Windows 10/11 全系内置；仅 Windows N/K 版（无 Media Feature Pack）
//! 缺失——`new()` 会失败，由调用方（decode.rs）转为带操作指引的错误提示。

use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::{
    CMSH264DecoderMFT, CODECAPI_AVLowLatencyMode, ICodecAPI, IMFMediaType, IMFSample, IMFTransform,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFStartup,
    MFVideoFormat_H264, MFVideoFormat_NV12, MFSTARTUP_LITE, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_INFO,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_VERSION,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::{VARIANT, VT_UI4};

use crate::decode::DecodedFrame;

/// 构造 VARIANT(UI4)（ICodecAPI::SetValue 参数；windows crate 未提供 From 辅助）。
///
/// 注意：`CODECAPI_AVLowLatencyMode` 等属性的值类型为 **VT_UI4**——用 VT_BOOL
/// 会被拒绝（实测报 `Invalid argument (VT_UI4 != pValue->vt)`），进而静默退回
/// 默认模式（解码器缓冲 30+ 帧才输出，直播追帧场景不可接受）。
fn variant_ui4(x: u32) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        let p = &mut v.Anonymous.Anonymous;
        p.vt = VT_UI4;
        p.Anonymous.ulVal = x;
    }
    v
}

/// 进程级 MF 初始化（COM MTA + MFStartup）。
///
/// 设计：`MFStartup` 为进程级（`OnceLock` 缓存成败——失败如 N/K 版缺 Media
/// Feature Pack 直接复用错误，不做无谓重试）。**COM 初始化是每线程的**：
/// 每次调用都执行 `CoInitializeEx`（已初始化返回 S_FALSE，幂等）——批量
/// 并行解码（多个会话各占一个线程）时，每个解码线程都需要自初始化 COM。
static MF_START: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();

fn ensure_mf_started() -> Result<(), String> {
    // RPC_E_CHANGED_MODE（线程已是 STA 等）容忍：继续用当前模式。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    MF_START
        .get_or_init(|| unsafe {
            // MFSTARTUP_LITE：仅启动核心（不初始化网络流媒体栈），解码器场景最轻。
            MFStartup(MF_VERSION, MFSTARTUP_LITE)
                .map_err(|e| format!("Media Foundation 初始化失败（系统可能缺少组件）: {e}"))
        })
        .clone()
}

/// 系统 H.264 解码 MFT 是否可用（Windows N/K 版缺 Media Feature Pack 时为否）。
pub fn available() -> bool {
    MfDecoder::new().is_ok()
}

/// MF H.264 解码器：一次会话持有 `IMFTransform`，逐访问单元（Annex-B）解码。
///
/// 仅限创建它的线程使用（COM 对象按 MTA 服务；本管线解码线程自持自用）。
pub struct MfDecoder {
    transform: IMFTransform,
    /// MFT 自行提供输出 sample（GetOutputStreamInfo 协商结果）。
    /// 为否时由本结构预分配输出 sample。
    mft_provides_samples: bool,
    /// 当前输出（编码）尺寸与 NV12 亮度行跨距（输出类型协商后确定）。
    width: u32,
    height: u32,
    /// 显示孔径尺寸（编码高度可能含 16 对齐填充，如 1080→1088）。
    /// 有显示孔径时只输出其区域，避免底部填充行显示为绿色条。
    disp_w: u32,
    disp_h: u32,
    /// NV12 亮度平面行跨距（字节）。输出类型的 MF_MT_DEFAULT_STRIDE，
    /// 缺失时退回宽度（copy-back 常规情形）。
    stride: u32,
    /// 预创建输出 sample（mft_provides_samples=false 时用；尺寸变化后重建）。
    out_sample: Option<IMFSample>,
}

// SAFETY: MfDecoder 只在解码线程（创建线程）内使用；标记 Send 以便放入
// `Box<dyn VideoDecoder>`（trait 对象本身不要求 Send，但为将来移动留余地）。
unsafe impl Send for MfDecoder {}

impl MfDecoder {
    /// 创建解码器并完成类型协商：
    /// 输入 = H.264(Annex-B)，输出 = NV12（系统内存、copy-back）。
    pub fn new() -> Result<Self, String> {
        ensure_mf_started()?;
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CMSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).map_err(|e| {
                    format!("创建 H.264 解码 MFT 失败（系统可能缺少 Media Feature Pack）: {e}")
                })?;

            // 低延迟模式（必须）：默认模式下 H.264 MFT 会缓冲 30+ 帧才输出
            // （社区多方实测），低延迟模式下逐帧即出（对追帧场景关键）。
            // 注意值类型为 VT_UI4（用 BOOL 会被拒——踩坑记录见 variant_ui4）。
            if let Ok(codec) = transform.cast::<ICodecAPI>() {
                match codec.SetValue(&CODECAPI_AVLowLatencyMode, &variant_ui4(1)) {
                    Ok(()) => log::info!("MF 低延迟模式已启用（CODECAPI_AVLowLatencyMode）"),
                    Err(e) => log::warn!("MF 低延迟模式设置失败（继续默认模式）: {e}"),
                }
            }

            // 输入类型：H.264 Annex-B（官方要求开始码格式，与 avcc_to_annexb 输出一致）。
            let it = MFCreateMediaType().map_err(|e| format!("MFCreateMediaType: {e}"))?;
            it.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| format!("SetGUID(输入): {e}"))?;
            it.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .map_err(|e| format!("SetGUID(H264): {e}"))?;
            transform
                .SetInputType(0, &it, 0)
                .map_err(|e| format!("SetInputType(H264): {e}"))?;

            let mut dec = Self {
                transform,
                mft_provides_samples: false,
                width: 0,
                height: 0,
                disp_w: 0,
                disp_h: 0,
                stride: 0,
                out_sample: None,
            };
            dec.negotiate_output()?;

            // 流控消息：开始流式解码（解码器可开始输出）。
            let t = &dec.transform;
            let _ = t.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = t.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = t.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
            Ok(dec)
        }
    }

    /// 选择 NV12 输出类型并刷新尺寸/跨距与输出 sample 提供方。
    /// 首次协商与 `MF_E_TRANSFORM_STREAM_CHANGE` 后复用同一路径。
    unsafe fn negotiate_output(&mut self) -> Result<(), String> {
        // 枚举可用输出类型，优先 NV12（H.264 decoder 的默认输出即 NV12）。
        let mut chosen: Option<IMFMediaType> = None;
        for idx in 0..16u32 {
            let Ok(t) = self.transform.GetOutputAvailableType(0, idx) else {
                break;
            };
            if let Ok(sub) = t.GetGUID(&MF_MT_SUBTYPE) {
                if sub == MFVideoFormat_NV12 {
                    chosen = Some(t);
                    break;
                }
            }
        }
        let ot = chosen.ok_or_else(|| "H.264 解码器未提供 NV12 输出类型".to_string())?;
        self.transform
            .SetOutputType(0, &ot, 0)
            .map_err(|e| format!("SetOutputType(NV12): {e}"))?;

        // 尺寸：帧尺寸在 STREAM_CHANGE 后才有值（首帧前可能为 0，属正常，
        // 解码到第一帧时会再次协商）。MF_MT_FRAME_SIZE = (宽 << 32) | 高。
        let frame_size = ot.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
        self.width = (frame_size >> 32) as u32;
        self.height = (frame_size & 0xFFFF_FFFF) as u32;
        // 显示孔径：编码高度常按 16 对齐（如 1080→1088），孔径给出真实显示尺寸；
        // 无孔径（全 0）则视为编码尺寸（不裁剪）。
        self.disp_w = 0;
        self.disp_h = 0;
        let mut ap = [0u8; 16];
        let mut ap_len = 0u32;
        if ot
            .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut ap, Some(&mut ap_len))
            .is_ok()
            && ap_len >= 16
        {
            // MFVideoArea = OffsetX(4) OffsetY(4) cx(4) cy(4)。
            let cx = u32::from_le_bytes([ap[8], ap[9], ap[10], ap[11]]);
            let cy = u32::from_le_bytes([ap[12], ap[13], ap[14], ap[15]]);
            if cx > 0 && cy > 0 && cx <= self.width && cy <= self.height {
                self.disp_w = cx;
                self.disp_h = cy;
            }
        }
        if self.disp_w == 0 || self.disp_h == 0 {
            self.disp_w = self.width;
            self.disp_h = self.height;
        }
        if (self.disp_w, self.disp_h) != (self.width, self.height) {
            log::info!(
                "MF 显示孔径 {}x{}（编码 {}x{}，裁剪填充行）",
                self.disp_w,
                self.disp_h,
                self.width,
                self.height
            );
        }
        self.stride = ot
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .unwrap_or(self.width)
            .max(self.width);

        let info: MFT_OUTPUT_STREAM_INFO = self
            .transform
            .GetOutputStreamInfo(0)
            .map_err(|e| format!("GetOutputStreamInfo: {e}"))?;
        self.mft_provides_samples =
            (info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
        log::debug!(
            "MF 输出协商: {}x{}（框架尺寸 {}, 跨距 {}），dwFlags=0x{:x}，cbSize={}，MFT 提供 sample={}",
            self.width,
            self.height,
            frame_size,
            self.stride,
            info.dwFlags,
            info.cbSize,
            self.mft_provides_samples
        );
        // 尺寸变化后旧输出 sample 作废。
        self.out_sample = None;
        Ok(())
    }

    /// 送一个 Annex-B 访问单元（可含 SPS/PPS 等多个 NAL），返回产出的帧。
    ///
    /// 与 openh264 路径的语义对齐：0..n 帧（低延迟模式下通常 0 或 1 帧）。
    pub fn decode(&mut self, annexb: &[u8], ts_ms: u32) -> Result<Vec<DecodedFrame>, String> {
        if annexb.is_empty() {
            return Ok(Vec::new());
        }
        unsafe {
            // 输入 sample：拷贝访问单元到 MF memory buffer。
            let buf = MFCreateMemoryBuffer(annexb.len() as u32)
                .map_err(|e| format!("MFCreateMemoryBuffer: {e}"))?;
            {
                let mut p = std::ptr::null_mut();
                buf.Lock(&mut p, None, None)
                    .map_err(|e| format!("Lock(输入): {e}"))?;
                std::ptr::copy_nonoverlapping(annexb.as_ptr(), p, annexb.len());
                buf.Unlock().map_err(|e| format!("Unlock(输入): {e}"))?;
            }
            buf.SetCurrentLength(annexb.len() as u32)
                .map_err(|e| format!("SetCurrentLength: {e}"))?;
            let sample = MFCreateSample().map_err(|e| format!("MFCreateSample: {e}"))?;
            sample
                .AddBuffer(&buf)
                .map_err(|e| format!("AddBuffer: {e}"))?;
            // 媒体时间戳（100ns 单位）——低延迟模式下主要用于诊断。
            let _ = sample.SetSampleTime(ts_ms as i64 * 10_000);

            // 输入：MFT 输入队列满（NOTACCEPTING）时先排空输出再重试一次。
            let mut hr = self.transform.ProcessInput(0, &sample, 0);
            if hr
                .as_ref()
                .err()
                .is_some_and(|e| e.code() == MF_E_NOTACCEPTING)
            {
                let _ = self.drain_output()?;
                hr = self.transform.ProcessInput(0, &sample, 0);
            }
            hr.map_err(|e| format!("ProcessInput: {e}"))?;
            log::debug!("MF ProcessInput 接受 {} 字节", annexb.len());

            self.drain_output()
        }
    }

    /// 排空：通知解码器「无更多输入」并取回全部剩余帧。
    /// 一次性解码（测试/文件）用；直播流无结束，不应调用。
    pub fn drain(&mut self) -> Result<Vec<DecodedFrame>, String> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(|e| format!("DRAIN: {e}"))?;
            self.drain_output()
        }
    }

    /// 循环取回所有可用输出帧（直到 NEED_MORE_INPUT）。
    unsafe fn drain_output(&mut self) -> Result<Vec<DecodedFrame>, String> {
        let mut frames = Vec::new();
        loop {
            let mut odb = MFT_OUTPUT_DATA_BUFFER::default();
            if !self.mft_provides_samples {
                if self.out_sample.is_none() {
                    self.out_sample = Some(self.create_out_sample()?);
                }
                let s = self.out_sample.clone().expect("输出 sample 刚创建");
                // **复用前必须把 buffer 的 currentLength 重置为 0**：H.264 MFT
                // 只在输出 buffer「为空」时接受（否则 ProcessOutput 报
                // `CopyDecodedFrame failed`，0x80004005——社区确证的踩坑点）。
                if let Ok(buf) = s.ConvertToContiguousBuffer() {
                    let _ = buf.SetCurrentLength(0);
                }
                odb.pSample = std::mem::ManuallyDrop::new(Some(s));
            }
            let mut outputs = [odb];
            let mut status = 0u32;
            let hr = self.transform.ProcessOutput(0, &mut outputs, &mut status);
            match hr {
                Ok(()) => {
                    // 从返回的 sample 读取 NV12（MFT 提供或我们预分配，两者都有效）。
                    let s: Option<IMFSample> =
                        std::mem::ManuallyDrop::take(&mut outputs[0].pSample);
                    if let Some(s) = s {
                        match self.read_nv12(&s) {
                            Ok(Some(f)) => frames.push(f),
                            Ok(None) => {} // 尺寸未知（首帧前）——忽略该帧
                            Err(e) => log::warn!("MF 输出帧读取失败: {e}"),
                        }
                    }
                    // MFT 提供的 sample 由我们负责释放（drop 即 Release）。
                    // 我们预分配的 sample 由 self.out_sample 持有，继续复用。
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // 输出类型需重新协商（首帧/分辨率变化）。必须重新 SetOutputType。
                    self.negotiate_output()?;
                    continue;
                }
                Err(e) => return Err(format!("ProcessOutput: {e}")),
            }
        }
        Ok(frames)
    }

    /// 预分配输出 sample（NV12 系统内存）。大小取 GetOutputStreamInfo.cbSize
    /// （不足时保守回退 宽×高×2）。
    unsafe fn create_out_sample(&self) -> Result<IMFSample, String> {
        let info: MFT_OUTPUT_STREAM_INFO = self
            .transform
            .GetOutputStreamInfo(0)
            .map_err(|e| format!("GetOutputStreamInfo: {e}"))?;
        let need = if info.cbSize > 0 {
            info.cbSize
        } else {
            (self.width * self.height * 2).max(1920 * 1080)
        };
        let buf =
            MFCreateMemoryBuffer(need).map_err(|e| format!("MFCreateMemoryBuffer(输出): {e}"))?;
        let sample = MFCreateSample().map_err(|e| format!("MFCreateSample(输出): {e}"))?;
        sample
            .AddBuffer(&buf)
            .map_err(|e| format!("AddBuffer(输出): {e}"))?;
        Ok(sample)
    }

    /// 读取 NV12 输出并转为紧凑 I420：Y 平面逐行拷贝（跳跨距），UV 交错区
    /// 拆分为 U/V 两个平面。返回 `Ok(None)` 表示尺寸未知（忽略此帧）。
    unsafe fn read_nv12(&self, sample: &IMFSample) -> Result<Option<DecodedFrame>, String> {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 {
            return Ok(None);
        }
        let buf = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| format!("ConvertToContiguousBuffer: {e}"))?;
        let mut p = std::ptr::null_mut::<u8>();
        let mut cur = 0u32;
        buf.Lock(&mut p, None, Some(&mut cur))
            .map_err(|e| format!("Lock(输出): {e}"))?;

        let result = (|| {
            let (w, h) = (w as usize, h as usize);
            let stride = self.stride.max(w as u32) as usize;
            // 输出尺寸 = 显示孔径（去除编码对齐填充行，避免绿条）。
            let ow = (self.disp_w as usize).min(w).max(2);
            let oh = (self.disp_h as usize).min(h).max(2);
            let (cw, ch) = (ow / 2, oh / 2);
            // 尺寸合法性（防 MFT 输出异常数据）。
            if (cur as usize) < stride * h + stride * (h / 2) {
                return Err(format!(
                    "NV12 缓冲不足：{cur} 字节 < 需要 {} 字节",
                    stride * h + stride * (h / 2)
                ));
            }
            let mut y = Vec::with_capacity(ow * oh);
            for row in 0..oh {
                let base = p.add(row * stride);
                y.extend_from_slice(std::slice::from_raw_parts(base, ow));
            }
            let uv_base = p.add(stride * h);
            let mut u = vec![0u8; cw * ch];
            let mut v = vec![0u8; cw * ch];
            for row in 0..ch {
                let rowp = std::slice::from_raw_parts(uv_base.add(row * stride), ow);
                let u_row = &mut u[row * cw..(row + 1) * cw];
                let v_row = &mut v[row * cw..(row + 1) * cw];
                // zip 迭代 + 定长 chunk 拆解：比逐字节 push 快数倍（无容量检查、
                // 利于向量化；测试/低配环境的差距最明显）。
                for ((du, dv), src) in u_row
                    .iter_mut()
                    .zip(v_row.iter_mut())
                    .zip(rowp.as_chunks::<2>().0)
                {
                    *du = src[0];
                    *dv = src[1];
                }
            }
            Ok(DecodedFrame {
                w: ow as u32,
                h: oh as u32,
                y,
                u,
                v,
            })
        })();
        let _ = buf.Unlock();
        result.map(Some)
    }
}
