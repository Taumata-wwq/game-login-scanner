//! Android 屏幕捕获门面（对位 Windows screenshot.rs）。
//!
//! Windows：Win32 GDI 截屏；Android：MediaProjection + ImageReader（阶段 2 落地）。
//! 当前为编译门面：类型与函数签名与 Windows 版一致，功能待阶段 2 接入。

/// 一帧截图的 RGB 数据（预览兜底路径）。
pub struct Frame {
    pub rgb: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// 一帧截图的灰度数据（紧凑布局：行跨距 = w）。
pub struct GrayFrame {
    pub gray: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// 显示器/屏幕矩形列表（Android 为单一屏幕，阶段 2 接 DisplayManager 返回真实尺寸）。
pub fn screen_rects() -> Vec<(i32, i32, i32, i32)> {
    // TODO(阶段2)：从 Kotlin 插件取 DisplayManager 屏幕尺寸。
    Vec::new()
}

/// 截取指定区域并返回 RGB。
pub fn capture_region(_x: i32, _y: i32, _w: i32, _h: i32) -> Option<Frame> {
    // TODO(阶段2)：MediaProjection ImageReader 取帧。
    None
}

/// 截取指定区域并直接转灰度。
pub fn capture_region_gray(_x: i32, _y: i32, _w: i32, _h: i32) -> Option<GrayFrame> {
    // TODO(阶段2)：MediaProjection ImageReader 取帧 + 灰度转换。
    None
}
