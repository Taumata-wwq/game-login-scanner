//! 视频解码器抽象：Media Foundation H.264 硬解（系统组件）。
//!
//! 选型沿革与依据（2026-10-11 调研 → 2026-10-12 落地）：
//! - 原实现为 openh264 单线程软解（Windows 上多线程必崩），1080p60 单线程
//!   19~24ms/帧跟不上流帧间隔，追帧/降清晰度常态化；
//! - 系统自带 H.264 解码 MFT（见 mf_decode.rs）：**提供 DXGI 设备管理器后走
//!   DXVA 硬解**（输出 D3D11 纹理、回读转 I420），CPU 占用大幅下降、延迟更低，
//!   且**零体积增量**（系统组件）；设备管理器不可用时自动回退软解（行为同旧版）；
//! - 稳定性验证（移除软解回退的依据）：300 个真实直播间批量解码测试
//!   （5 游戏分区，tests/live_batch.rs）**零解码错误**，覆盖 640×368~4K 分辨率与
//!   60~77fps 高帧率流；本地自校验与直播长跑亦全部通过。
//!   → 2026-10-12 移除 openh264 回退（用户确认），依赖树同步清理。
//!
//! 平台要求：Windows 10/11 全系内置 H.264 解码 MFT；**Windows N/K 版**
//! 需安装 Media Feature Pack，否则 `new_decoder()` 返回明确错误（前端展示）。

#[cfg(windows)]
use crate::mf_decode::MfDecoder;

/// 解码产出的一帧：**紧凑 I420**（三平面，行跨距 = 宽）。
pub struct DecodedFrame {
    pub w: u32,
    pub h: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

/// YUV420 输出缓冲的色度排布（不同平台/编解码器不同）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChromaLayout {
    /// 平面（I420 / YUV420P）：U、V 各占连续区域（U 在前）。
    Planar,
    /// 半平面（NV12 / NV21）：UV 交错存储。
    SemiPlanar,
}

/// 从解码器输出的原始 YUV420 缓冲中提取**紧凑 I420** 三平面（行跨距恒为宽）。
///
/// 输入缓冲布局由 `w/h/stride/slice_h/layout` 描述（厂商相关的行跨距与分配高度）。
/// **Y（亮度）逐行严格取前 w 字节**——这是扫码唯一依赖的像素路径，必须正确；
/// U/V 依 layout 尽力解析，越界或未知处填中性灰 128（预览偏灰，不影响扫码识别）。
///
/// 该函数跨平台（不依赖任何 Android API），便于在桌面单元测试覆盖 stride 逐行
/// 拷贝这一易错环节（真机像素路径无法在 CI 覆盖）。
pub fn planes_from_yuv420(
    w: usize,
    h: usize,
    stride: usize,
    slice_h: usize,
    layout: ChromaLayout,
    data: &[u8],
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = w.max(1);
    let h = h.max(1);
    let stride = stride.max(w);
    let slice_h = slice_h.max(h);
    // ---- Y（亮度）：逐行取前 w 字节 ----
    let mut y = vec![0u8; w * h];
    for row in 0..h {
        let s = row * stride;
        if s + w > data.len() {
            break;
        }
        y[row * w..row * w + w].copy_from_slice(&data[s..s + w]);
    }
    // ---- 色度（U/V）：2×2 下采样 ----
    let cw = w / 2;
    let ch = h / 2;
    let mut u = vec![128u8; cw * ch];
    let mut v = vec![128u8; cw * ch];
    if cw == 0 || ch == 0 {
        return (y, u, v);
    }
    // Y 平面占用的字节数（按分配高度）；色度紧随其后。
    let y_plane = stride.saturating_mul(slice_h);
    match layout {
        // 平面（I420）：U、V 各占 (stride/2)×ch。
        ChromaLayout::Planar => {
            let cstride = stride / 2;
            let csize = cstride.saturating_mul(ch);
            for row in 0..ch {
                let us = y_plane + row * cstride;
                let vs = y_plane + csize + row * cstride;
                if us + cw <= data.len() {
                    u[row * cw..row * cw + cw].copy_from_slice(&data[us..us + cw]);
                }
                if vs + cw <= data.len() {
                    v[row * cw..row * cw + cw].copy_from_slice(&data[vs..vs + cw]);
                }
            }
        }
        // 半平面（NV12/NV21 及厂商变体，最常见）：UV 交错，行跨距 = stride。
        ChromaLayout::SemiPlanar => {
            let uv_stride = stride.max(cw * 2);
            for row in 0..ch {
                let base = y_plane + row * uv_stride;
                for col in 0..cw {
                    let idx = base + col * 2;
                    if idx + 1 < data.len() {
                        // NV12：U 偶、V 奇。（NV21 会 U/V 互换——预览偏色，扫码不受影响。）
                        u[row * cw + col] = data[idx];
                        v[row * cw + col] = data[idx + 1];
                    }
                }
            }
        }
    }
    (y, u, v)
}

/// 解码器统一接口。
pub trait VideoDecoder {
    /// 送一个 Annex-B 访问单元（可含 SPS/PPS 等多个 NAL），返回产出的帧（0..n）。
    /// `ts_ms` 为媒体时间戳（毫秒；写入 MF sample 供诊断）。
    fn decode(&mut self, annexb: &[u8], ts_ms: u32) -> Result<Vec<DecodedFrame>, String>;

    /// 实现名（日志/诊断用）。
    fn name(&self) -> &'static str;
}

/// 让 `Box<dyn VideoDecoder>` 直接满足本 trait（调用点无需手动解引用）。
impl VideoDecoder for Box<dyn VideoDecoder> {
    fn decode(&mut self, annexb: &[u8], ts_ms: u32) -> Result<Vec<DecodedFrame>, String> {
        (**self).decode(annexb, ts_ms)
    }

    fn name(&self) -> &'static str {
        (**self).name()
    }
}

/// 解码器工厂：创建 Media Foundation H.264 硬解器。
///
/// 失败时返回带操作指引的错误（供直播启动错误提示直接展示）：
/// - Windows N/K 版缺少 Media Feature Pack 时 MFT 不可用；
/// - 极端老驱动初始化异常。
#[cfg(windows)]
pub fn new_decoder() -> Result<Box<dyn VideoDecoder>, String> {
    match MfDecoder::new() {
        Ok(inner) => {
            log::info!(
                "直播解码器：Media Foundation H.264（{}，系统组件，零体积）",
                if inner.dxva_active() {
                    "DXVA 硬解"
                } else {
                    "软解"
                }
            );
            Ok(Box::new(HwDecoder { inner }))
        }
        Err(e) => Err(format!(
            "系统 H.264 解码组件不可用（{e}）。\
             本工具依赖 Windows 内置的 Media Foundation 解码器；\
             若为 Windows N/K 版，请安装「媒体功能包（Media Feature Pack）」后重试"
        )),
    }
}

/// Android：MediaCodec 硬件解码（见 android/mediacodec_decode.rs）。
#[cfg(target_os = "android")]
pub fn new_decoder() -> Result<Box<dyn VideoDecoder>, String> {
    match crate::android::mediacodec_decode::MediaCodecDecoder::new() {
        Ok(inner) => {
            log::info!("直播解码器：Android MediaCodec H.264 硬解（系统组件）");
            Ok(Box::new(inner))
        }
        Err(e) => Err(format!("MediaCodec 解码器初始化失败：{e}")),
    }
}

// ---------- Media Foundation 硬解（Windows）----------

#[cfg(windows)]
struct HwDecoder {
    inner: MfDecoder,
}

#[cfg(windows)]
impl VideoDecoder for HwDecoder {
    fn decode(&mut self, annexb: &[u8], ts_ms: u32) -> Result<Vec<DecodedFrame>, String> {
        self.inner.decode(annexb, ts_ms)
    }

    fn name(&self) -> &'static str {
        if self.inner.dxva_active() {
            "MF(DXVA)"
        } else {
            "MF(软解)"
        }
    }
}
