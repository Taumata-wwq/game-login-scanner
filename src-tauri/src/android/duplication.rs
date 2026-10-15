//! Android 取帧门面（对位 Windows duplication.rs）。
//!
//! Windows：DXGI 桌面复制（拉模型 + 脏矩形瓦片）；
//! Android：MediaProjection 帧总线（推模型，见 [`super::capture`]）。
//! 类型与函数签名与 Windows 版一致，调用点（lib.rs / scan.rs / qr.rs）零改动。

use super::capture;

/// 显示器键（虚拟桌面矩形）。Android 单屏：恒为 (0, 0, w, h)。
pub type MonitorKey = (i32, i32, i32, i32);

/// 单个变化的瓦片：(col, row, w, h, RGBA)。Android 无瓦片机制（仅保留类型对位）。
pub type TileUpdate = (u32, u32, u32, u32, Vec<u8>);
/// 瓦片增量帧：(降采样宽, 高, 变化瓦片列表)。
pub type TileDelta = (u32, u32, Vec<TileUpdate>);

/// 取帧不可用（未授权 / 未启动捕获 / 首帧未到达）。
#[derive(Debug)]
pub struct Unavailable;

/// 预览取帧结果。
pub enum PreviewOut {
    /// 新帧（RGBA + 宽 + 高）
    Frame(Vec<u8>, u32, u32),
    /// 帧未变化（消费者可沿用上一帧）
    Unchanged,
}

/// 预览取帧：`region` 为可视视口（屏幕物理坐标），降采样到 `tw` 宽（RGBA 输出）。
///
/// 变化检测用帧序号（有新帧才转换）；转换与裁剪在帧槽锁内完成。
pub fn preview_frame(region: MonitorKey, tw: u32) -> Result<PreviewOut, Unavailable> {
    let g = capture::lock();
    let f = g.as_ref().ok_or(Unavailable)?;
    if f.seq == capture::last_preview_seq() {
        return Ok(PreviewOut::Unchanged);
    }
    let (x, y, w, h) = capture::crop_of(region, f.w, f.h);
    let (rgba, ow, oh) = capture::rgba_crop_scale(&f.data, f.w as usize * 4, x, y, w, h, tw);
    capture::set_last_preview_seq(f.seq);
    Ok(PreviewOut::Frame(rgba, ow, oh))
}

/// 瓦片（拖拽底图）模式：Android 无脏矩形机制，no-op；
/// [`tile_frame`] 恒返回 Ok(None)，前端回退到普通预览路径。
pub fn set_tile_mode(_region: MonitorKey, _on: bool) {}

/// 瓦片增量帧：Android 无瓦片机制，恒返回 `Ok(None)`（无瓦片更新 → 前端回退普通预览）。
/// Err 仍如实反映帧总线不可用，避免上层把「无瓦片」误判成「可取帧」。
pub fn tile_frame(_region: MonitorKey, _max_tw: u32) -> Result<Option<TileDelta>, Unavailable> {
    let g = capture::lock();
    if g.is_none() {
        return Err(Unavailable);
    }
    Ok(None)
}

/// 预览 ROI 复位：Android 无 ROI 优化，no-op。
pub fn clear_preview_roi() {}

/// 扫码 ROI 复位：Android 无 ROI 优化，no-op。
pub fn clear_scan_roi(_region: MonitorKey) {}

/// 扫码取帧：画面内容自上次调用以来有变化时返回灰度帧；无变化返回 `None`。
///
/// `scale` 为步长降采样倍率（1 = 原分辨率，2 = 1/2；转换与降采样合并为一趟）。
/// 变化检测用采样指纹（全帧内容未变 → 一定无变化；全帧变了但区域内未变时
/// 至多多解码一次，无害）。Err = 帧总线不可用（未授权 / 首帧未到）。
pub fn scan_frame(
    region: MonitorKey,
    scale: u32,
) -> Result<Option<crate::screenshot::GrayFrame>, Unavailable> {
    let g = capture::lock();
    let f = g.as_ref().ok_or(Unavailable)?;
    if f.fingerprint == capture::last_scan_fp() {
        return Ok(None);
    }
    let (x, y, w, h) = capture::crop_of(region, f.w, f.h);
    let (gray, ow, oh) =
        capture::rgba_gray_stride(&f.data, f.w as usize * 4, x, y, w, h, scale as usize);
    capture::set_last_scan_fp(f.fingerprint);
    Ok(Some(crate::screenshot::GrayFrame { gray, w: ow, h: oh }))
}
