//! Android 剪贴板门面（对位 Windows clipboard.rs）。
//!
//! Windows：CF_DIB 手写解析；Android：阶段 2 接 ClipboardManager（读图能力有限，
//! 通常返回 None，由屏幕/摄像头路径兜底）。

/// 读取剪贴板位图，返回 (RGBA, 宽, 高)。Android 端暂不可用，返回 None。
pub fn read_image_rgba() -> Option<(Vec<u8>, u32, u32)> {
    // TODO(阶段2)：视需要接 Android ClipboardManager。
    None
}
