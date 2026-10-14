//! Android 取帧门面（对位 Windows duplication.rs）。
//!
//! Windows：DXGI 桌面复制；Android：MediaProjection 帧总线（阶段 2 落地）。
//! 类型与函数签名与 Windows 版一致，调用点（lib.rs / scan.rs）零改动。

/// 显示器键（虚拟桌面矩形）。
pub type MonitorKey = (i32, i32, i32, i32);

/// 单个变化的瓦片：(col, row, w, h, RGBA)。
pub type TileUpdate = (u32, u32, u32, u32, Vec<u8>);
/// 瓦片增量帧：(降采样宽, 高, 变化瓦片列表)。
pub type TileDelta = (u32, u32, Vec<TileUpdate>);

/// 取帧不可用（Android 未授权 / 未启动前台服务）。
#[derive(Debug)]
pub struct Unavailable;

/// 预览取帧结果。
pub enum PreviewOut {
    Frame(Vec<u8>, u32, u32),
    Unchanged,
}

pub fn preview_frame(_region: MonitorKey, _tw: u32) -> Result<PreviewOut, Unavailable> {
    // TODO(阶段2)：MediaProjection 取帧 + 降采样。
    Err(Unavailable)
}

pub fn set_tile_mode(_region: MonitorKey, _on: bool) {}

pub fn tile_frame(_region: MonitorKey, _max_tw: u32) -> Result<Option<TileDelta>, Unavailable> {
    Err(Unavailable)
}

pub fn clear_preview_roi() {}

pub fn clear_scan_roi(_region: MonitorKey) {}

/// 扫码取帧：有变化返回灰度帧，无变化返回 None。
pub fn scan_frame(
    _region: MonitorKey,
    _scale: u32,
) -> Result<Option<crate::screenshot::GrayFrame>, Unavailable> {
    // TODO(阶段2)：MediaProjection 取帧 + 灰度 + 步长降采样。
    Err(Unavailable)
}
