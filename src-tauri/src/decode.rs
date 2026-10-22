//! 视频解码器抽象：Media Foundation H.264 硬解（系统组件）。
//!
//! 选型依据：
//! - 系统自带 H.264 解码 MFT（mf_decode.rs）在提供 DXGI 设备管理器后走 **DXVA 硬解**
//!   （输出 D3D11 纹理、回读转 I420），CPU 占用与延迟都低，且**零体积增量**；
//!   设备管理器不可用时自动回退软解；
//! - 稳定性：300 个真实直播间批量解码（5 个游戏分区，tests/live_batch.rs）零错误，
//!   覆盖 640×368~4K 与 60~77fps 流；
//! - 早期的 openh264 单线程软解在 Windows 上多线程必崩，且 1080p60 需 19~24ms/帧、
//!   跟不上流帧间隔，已整体移除（依赖树同步清理）。
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

/// 从 Annex-B 字节流中解析 H.264 SPS 的显示宽高（像素）。
///
/// 用途：Android 的 `MediaCodec` 在 `configure` 时**必须**提供 width/height
/// （NDK 头 `AMediaCodec_configure` 与官方示例均如此），而解码器无法自行从
/// SPS 推导——故在收到首个含 SPS 的访问单元后解析出宽高再 configure。
///
/// 实现要点：找 NAL type 7（SPS），去 emulation prevention（`00 00 03`），
/// 按 H.264 语法用指数哥伦布读 `pic_width_in_mbs_minus1` /
/// `pic_height_in_map_units_minus1` / `frame_mbs_only_flag` / `frame_cropping`，
/// 换算为显示宽高。解析失败返回 `None`（调用方可退回经验值或报错）。
///
/// 该函数跨平台（纯字节运算），便于桌面单元测试覆盖。
pub fn sps_dimensions(annexb: &[u8]) -> Option<(u32, u32)> {
    for (start, nal) in annexb_nalus(annexb) {
        let _ = start;
        if !nal.is_empty() && nal[0] & 0x1F == 7 {
            if let Some(d) = parse_sps_dims(&nal[1..]) {
                return Some(d);
            }
        }
    }
    None
}

/// 把 Annex-B 字节流切分为 (起始码偏移, NAL 字节含头) 列表。
/// 起始码为 00 00 01 或 00 00 00 01。
fn annexb_nalus(data: &[u8]) -> Vec<(usize, &[u8])> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                starts.push((i, i + 3));
                i += 3;
                continue;
            }
            if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                starts.push((i, i + 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    let mut out = Vec::with_capacity(starts.len());
    for (idx, &(sc_pos, hdr_off)) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).map(|&(p, _)| p).unwrap_or(data.len());
        if hdr_off < end {
            out.push((sc_pos, &data[hdr_off..end]));
        }
    }
    out
}

/// 去除 SPS 的 emulation prevention bytes（00 00 03 → 00 00）。
fn unescape_sps(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0u32;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue; // 丢弃 emulation prevention byte
        }
        out.push(b);
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    out
}

/// 指数哥伦布比特读取器。
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // 比特位置
}
impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn read_bit(&mut self) -> Option<u32> {
        let byte = self.pos / 8;
        if byte >= self.data.len() {
            return None;
        }
        let bit = (self.data[byte] >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(bit as u32)
    }
    fn read_bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.read_bit()?;
        }
        Some(v)
    }
    /// 无符号指数哥伦布（ue(v)）。
    fn read_ue(&mut self) -> Option<u32> {
        let mut leading = 0u32;
        while self.read_bit()? == 0 {
            leading += 1;
            if leading > 31 {
                return None;
            }
        }
        if leading == 0 {
            return Some(0);
        }
        let rest = self.read_bits(leading)?;
        Some((1u32 << leading) - 1 + rest)
    }
    /// 有符号指数哥伦布（se(v)）。
    fn read_se(&mut self) -> Option<i32> {
        let ue = self.read_ue()?;
        let k = (ue as i32 + 1) / 2;
        Some(if ue % 2 == 1 { k } else { -k })
    }
}

/// 解析 SPS 载荷（已去 emulation prevention），返回显示宽高。
fn parse_sps_dims(sps: &[u8]) -> Option<(u32, u32)> {
    let data = unescape_sps(sps);
    let mut br = BitReader::new(&data);
    let profile_idc = br.read_bits(8)?;
    br.read_bits(8)?; // constraint_set flags + reserved
    br.read_bits(8)?; // level_idc
    br.read_ue()?; // seq_parameter_set_id
    let mut chroma_format_idc = 1u32;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = br.read_ue()?;
        if chroma_format_idc == 3 {
            br.read_bit()?; // separate_colour_plane_flag
        }
        br.read_ue()?; // bit_depth_luma_minus8
        br.read_ue()?; // bit_depth_chroma_minus8
        br.read_bit()?; // qpprime_y_zero_transform_bypass_flag
        if br.read_bit()? == 1 {
            // seq_scaling_matrix_present_flag：跳过 scaling list（简化：直接放弃解析）。
            return None;
        }
    }
    let _ = chroma_format_idc;
    br.read_ue()?; // log2_max_frame_num_minus4
    let pic_order_cnt_type = br.read_ue()?;
    if pic_order_cnt_type == 0 {
        br.read_ue()?; // log2_max_pic_order_cnt_lsb_minus4
    } else if pic_order_cnt_type == 1 {
        br.read_bit()?; // delta_pic_order_always_zero_flag
        br.read_se()?; // offset_for_non_ref_pic
        br.read_se()?; // offset_for_top_to_bottom_field
        let n = br.read_ue()?;
        for _ in 0..n {
            br.read_se()?;
        }
    }
    br.read_ue()?; // max_num_ref_frames
    br.read_bit()?; // gaps_in_frame_num_value_allowed_flag
    let pic_width_in_mbs_minus1 = br.read_ue()?;
    let pic_height_in_map_units_minus1 = br.read_ue()?;
    let frame_mbs_only_flag = br.read_bit()?;
    if frame_mbs_only_flag == 0 {
        br.read_bit()?; // mb_adaptive_frame_field_flag
    }
    br.read_bit()?; // direct_8x8_inference_flag
    let mut crop_l = 0u32;
    let mut crop_r = 0u32;
    let mut crop_t = 0u32;
    let mut crop_b = 0u32;
    if br.read_bit()? == 1 {
        // frame_cropping_flag
        crop_l = br.read_ue()?;
        crop_r = br.read_ue()?;
        crop_t = br.read_ue()?;
        crop_b = br.read_ue()?;
    }
    let width = (pic_width_in_mbs_minus1 + 1) * 16;
    let height = (pic_height_in_map_units_minus1 + 1) * 16 * (2 - frame_mbs_only_flag);
    // 裁剪单位（亮度）：取决于 chroma_format_idc（此处按 4:2:0 = 2 处理）。
    let crop_unit_x = if chroma_format_idc == 0 { 1 } else { 2 };
    let crop_unit_y = if chroma_format_idc == 0 {
        2
    } else {
        2 * (2 - frame_mbs_only_flag)
    };
    let width = width.saturating_sub((crop_l + crop_r) * crop_unit_x);
    let height = height.saturating_sub((crop_t + crop_b) * crop_unit_y);
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
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
