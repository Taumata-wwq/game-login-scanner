//! Android 屏幕捕获门面（对位 Windows screenshot.rs）。
//!
//! Windows：Win32 GDI 截屏（预览/扫码兜底）；Android：直接取帧总线的最新帧
//! （见 [`super::capture`]）——MediaProjection 的像素已在总线内，
//! 本文件是"另一个视角的读取入口"，保持与 Windows 版一致的调用契约。

use super::capture;

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

/// 显示器/屏幕矩形列表（Android 为单一屏幕）。
///
/// 优先取「授权时上报的屏幕尺寸」（首帧到达前即可用，scan loop 启动即需要）；
/// 其次取最新帧尺寸；均无（未授权）→ 空列表（调用方据此报"屏幕捕获失败"）。
pub fn screen_rects() -> Vec<(i32, i32, i32, i32)> {
    if let Some((w, h)) = capture::screen_size() {
        return vec![(0, 0, w as i32, h as i32)];
    }
    let g = capture::lock();
    if let Some(f) = g.as_ref() {
        return vec![(0, 0, f.w as i32, f.h as i32)];
    }
    Vec::new()
}

/// 截取指定区域并返回 RGB（取最新帧对应区域；无帧时 None）。
pub fn capture_region(x: i32, y: i32, w: i32, h: i32) -> Option<Frame> {
    let g = capture::lock();
    let f = g.as_ref()?;
    let (cx, cy, cw, ch) = capture::crop_of((x, y, w, h), f.w, f.h);
    let (rgb, ow, oh) = rgba_to_rgb(&f.data, f.w as usize * 4, cx, cy, cw, ch);
    Some(Frame { rgb, w: ow, h: oh })
}

/// 截取指定区域并直接转灰度（取最新帧；无帧时 None）。
pub fn capture_region_gray(x: i32, y: i32, w: i32, h: i32) -> Option<GrayFrame> {
    let g = capture::lock();
    let f = g.as_ref()?;
    let (cx, cy, cw, ch) = capture::crop_of((x, y, w, h), f.w, f.h);
    let (gray, ow, oh) = capture::rgba_gray_stride(&f.data, f.w as usize * 4, cx, cy, cw, ch, 1);
    Some(GrayFrame { gray, w: ow, h: oh })
}

/// RGBA 裁剪 → RGB（丢弃 alpha 通道）。
fn rgba_to_rgb(
    rgba: &[u8],
    pitch: usize,
    cx: usize,
    cy: usize,
    cw: u32,
    ch: u32,
) -> (Vec<u8>, u32, u32) {
    let mut out = Vec::with_capacity(cw as usize * ch as usize * 3);
    for y in 0..ch as usize {
        let row = &rgba[(cy + y) * pitch + cx * 4..];
        for x in 0..cw as usize {
            let i = x * 4;
            out.push(row[i]);
            out.push(row[i + 1]);
            out.push(row[i + 2]);
        }
    }
    (out, cw, ch)
}
