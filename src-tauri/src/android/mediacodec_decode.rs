//! Android MediaCodec H.264 硬件解码（实现 crate::decode::VideoDecoder）。
//!
//! 与桌面端 mf_decode.rs 对位：
//! - 桌面：Media Foundation MFT（DXVA 硬解）→ NV12 回读 → 紧凑 I420；
//! - Android：MediaCodec（ByteBuffer 模式，NDK \`AMediaCodec\`）→ 输出 YUV → 紧凑 I420。
//!
//! 选型依据（docs/安卓端开发文档.md 第 2.2 / 10.2 节）：
//! - **ByteBuffer 模式**（\`AMediaCodec_configure(surface = NULL)\`）：NDK 的 \`AMediaCodec\`
//!   没有 Java 端的 \`getOutputImage\`（该 API 仅存在于 Java SDK），若走 Surface 则像素
//!   只能经 \`AImageReader\` 取回——链路更长且需 Kotlin 插件。本实现走 ByteBuffer：
//!   \`dequeue_output\` 直接拿到解码像素，Y（亮度）平面用于 rxing 扫码，U/V 尽力解析。
//! - **低延迟**：configure 时设 \`low-latency = 1\`（API 30+；低版本忽略）——默认模式下
//!   解码器会缓冲多帧才输出，直播追帧场景不可接受。
//! - 同步 \`dequeue_*\` 模式（非异步 callback）：解码线程自持自用，最简单可控。
//!
//! **两阶段初始化**（真机实测修正）：\`configure\` **必须**提供 width/height（NDK 要求，
//! 解码器不会自行从 SPS 推导——缺失会返回 \`ErrorUnknown\`）。故 \`new()\` 只创建 codec，
//! 首个含 SPS 的访问单元到达时解析出宽高再 configure + start。
//!
//! 输出布局：ByteBuffer 的 YUV 排布**设备/编解码器相关**（NDK 文档明确警告不可移植）。
//! 本实现按输出 MediaFormat 的 \`color-format\` 键分支解析：半平面（NV12/NV21，最常见）
//! 与平面（I420）；\`stride\` / \`slice-height\` 用于逐行定位。**Y 平面严格正确**（扫码
//! 只需亮度）；U/V 在未知格式下退回中性灰 128（预览偏灰，不影响扫码识别）。

use crate::decode::{sps_dimensions, ChromaLayout, DecodedFrame, VideoDecoder};
use anodecs::{
    CodecOutputBuffer, ColorFormat, DequeueInputError, DequeueOutputError, MediaCodec, MediaFormat,
};

/// 输出布局（随 \`OutputFormatChanged\` 刷新；仅在收到首个输出帧后有效）。
#[derive(Clone, Copy)]
struct OutLayout {
    w: usize,
    h: usize,
    /// Y 平面行跨距（字节）——通常 ≥ w（对齐到 16/32/64）。
    stride: usize,
    /// 分配高度（\`slice-height\`）——Y 平面实际占用的行数。
    slice_h: usize,
    color: ColorFormat,
}

pub struct MediaCodecDecoder {
    /// 创建后即存在；configure 延迟到解析出 SPS 宽高。
    codec: MediaCodec,
    started: bool,
    layout: Option<OutLayout>,
}

impl MediaCodecDecoder {
    /// 创建 H.264 解码器句柄（**尚未 configure/start**）。对应桌面端 \`MfDecoder::new()\`。
    pub fn new() -> Result<Self, String> {
        let codec = MediaCodec::create_decoder("video/avc")
            .map_err(|e| format!("MediaCodec 创建 H.264 解码器失败: {e:?}"))?;
        log::info!("MediaCodec H.264 解码器已创建（等待 SPS 后 configure）");
        Ok(Self {
            codec,
            started: false,
            layout: None,
        })
    }

    /// 用解析出的宽高 configure + start（仅一次）。
    fn start_with(&mut self, w: u32, h: u32) -> Result<(), String> {
        let mut fmt = MediaFormat::new().map_err(|e| format!("MediaFormat 创建失败: {e:?}"))?;
        // 注意：anodecs 0.1.11 把 AMediaFormat_setString/setInt32 的 FFI 声明为 -> bool，
        // 但 NDK 头里二者均返回 **void**——返回值是寄存器残留垃圾（随机真/假），
        // **不可用于判成败**（真机实测：检查 set_string 返回值会误判初始化失败）。
        let _ = fmt.set_string("mime", "video/avc");
        let _ = fmt.set_i32("width", w as i32);
        let _ = fmt.set_i32("height", h as i32);
        // 低延迟（API 30+ 生效；低版本忽略）。值类型 i32=1。
        let _ = fmt.set_i32("low-latency", 1);
        self.codec
            .init(&fmt, None, 0)
            .map_err(|e| format!("MediaCodec 配置失败（{w}x{h}）: {e:?}"))?;
        self.codec
            .start()
            .map_err(|e| format!("MediaCodec 启动失败: {e:?}"))?;
        self.started = true;
        log::info!("MediaCodec H.264 解码器已启动（{w}x{h}，ByteBuffer 模式，低延迟请求）");
        Ok(())
    }

    /// 喂入一个 Annex-B 访问单元（可含 SPS/PPS + IDR 等）到一个输入缓冲。
    fn feed(&mut self, annexb: &[u8], ts_ms: u32) -> Result<(), String> {
        for _ in 0..50 {
            match self.codec.dequeue_input(10_000) {
                Ok(mut buf) => {
                    let (ptr, cap) = buf.buffer();
                    if cap < annexb.len() {
                        buf.cancel();
                        return Err(format!(
                            "MediaCodec 输入缓冲不足（容量 {cap} < 需 {}）",
                            annexb.len()
                        ));
                    }
                    // SAFETY: ptr 指向 codec 输入缓冲（cap 字节），本函数独占该缓冲
                    // 直到 drop（自动 queue）；annexb 与之不重叠。
                    unsafe {
                        std::ptr::copy_nonoverlapping(annexb.as_ptr(), ptr, annexb.len());
                    }
                    buf.set_write_size(annexb.len());
                    buf.set_time(ts_ms as u64 * 1000);
                    return Ok(()); // buf drop → 自动 queue_input
                }
                Err(DequeueInputError::TryAgainLater) => continue,
                Err(DequeueInputError::CodecError(e)) => {
                    return Err(format!("MediaCodec dequeue_input 错误: {e:?}"));
                }
            }
        }
        Err("MediaCodec 输入缓冲持续不可用（50 次重试失败）".to_string())
    }

    /// 取回当前所有可用输出帧（非阻塞）。
    fn drain(&mut self) -> Result<Vec<DecodedFrame>, String> {
        let mut frames = Vec::new();
        loop {
            match self.codec.dequeue_output(0) {
                Ok(buf) => {
                    if let Some(f) = Self::extract(&buf, &mut self.layout) {
                        frames.push(f);
                    }
                }
                Err(DequeueOutputError::TryAgainLater) => break,
                Err(DequeueOutputError::OutputFormatChanged) => continue,
                Err(DequeueOutputError::OutputBuffersChanged) => continue,
                Err(DequeueOutputError::CodecError(e)) => {
                    return Err(format!("MediaCodec dequeue_output 错误: {e:?}"));
                }
            }
        }
        Ok(frames)
    }

    /// 从输出缓冲解析一帧（Y 严格；U/V 依 color-format 尽力）。
    fn extract(buf: &CodecOutputBuffer, layout: &mut Option<OutLayout>) -> Option<DecodedFrame> {
        let fmt = buf.format();
        let w = fmt.get_i32("width")? as usize;
        let h = fmt.get_i32("height")? as usize;
        if w == 0 || h == 0 {
            return None;
        }
        let stride = fmt
            .get_i32("stride")
            .map(|s| s as usize)
            .filter(|&s| s >= w)
            .unwrap_or(w);
        let slice_h = fmt
            .get_i32("slice-height")
            .map(|s| s as usize)
            .filter(|&s| s >= h)
            .unwrap_or(h);
        let color = fmt.color_format().unwrap_or(ColorFormat::Yuv420SemiPlanar);
        let l = OutLayout {
            w,
            h,
            stride,
            slice_h,
            color,
        };
        *layout = Some(l);
        let data = buf.buffer_slice()?;
        Some(build_frame(&l, data))
    }
}

impl VideoDecoder for MediaCodecDecoder {
    fn decode(&mut self, annexb: &[u8], ts_ms: u32) -> Result<Vec<DecodedFrame>, String> {
        if annexb.is_empty() {
            return Ok(Vec::new());
        }
        // 首次：从 SPS 解析宽高并 configure + start。
        if !self.started {
            let (w, h) = sps_dimensions(annexb).ok_or_else(|| {
                "MediaCodec：首个访问单元中未找到可解析的 SPS（无法确定尺寸）".to_string()
            })?;
            self.start_with(w, h)?;
        }
        self.feed(annexb, ts_ms)?;
        self.drain()
    }

    fn name(&self) -> &'static str {
        "MediaCodec"
    }
}

/// 输出缓冲 → 紧凑 I420（像素提取复用跨平台纯函数 \`decode::planes_from_yuv420\`，
/// 其 Y 路径可被桌面单元测试覆盖）。
fn build_frame(l: &OutLayout, data: &[u8]) -> DecodedFrame {
    let layout = match l.color {
        // 平面（I420 / YUV420P）：U、V 各占连续区域。
        ColorFormat::Yuv420Planar | ColorFormat::Yuv420PackedPlanar => ChromaLayout::Planar,
        // 半平面（NV12/NV21 及厂商变体，最常见）+ 未知格式：按半平面尽力解析。
        _ => ChromaLayout::SemiPlanar,
    };
    let (y, u, v) = crate::decode::planes_from_yuv420(l.w, l.h, l.stride, l.slice_h, layout, data);
    DecodedFrame {
        w: l.w as u32,
        h: l.h as u32,
        y,
        u,
        v,
    }
}
