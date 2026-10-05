//! 区域截图（screenshots crate），输出 RGB / RGBA 帧。
//!
//! 坐标系约定（关键）：
//! - 传入的 x/y/w/h 为「屏幕虚拟桌面绝对坐标，物理像素」（与 Tauri 窗口
//!   `outer_position()`/`outer_size()` 一致）；
//! - screenshots 0.8 的 `Screen::capture_area(x, y, w, h)` 期望的是
//!   「相对该显示器原点的逻辑像素」（内部会乘以 scale_factor），
//!   因此必须先定位目标显示器并做换算，直接传绝对物理坐标在
//!   高 DPI / 副屏场景会截到错误区域。

/// 一帧截图的 RGB 数据。
pub struct Frame {
    pub rgb: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// 一帧截图的原始 RGBA 数据（预览直接输出 RGBA，避免二次转换）。
pub struct RgbaFrame {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// 截取虚拟桌面指定区域（绝对物理像素），返回原始 RGBA，失败返回 None。
pub fn capture_region_rgba(x: i32, y: i32, w: i32, h: i32) -> Option<RgbaFrame> {
    let w = w.max(1);
    let h = h.max(1);
    let screens = screenshots::Screen::all().ok()?;
    if screens.is_empty() {
        return None;
    }
    let cx = x + w / 2;
    let cy = y + h / 2;
    // 找到包含区域中心的显示器；找不到（悬在界外）时退回包含左上角的，再退回第一块屏。
    let screen = screens
        .iter()
        .find(|s| {
            let (sx, sy) = (s.display_info.x, s.display_info.y);
            let (sw, sh) = (s.display_info.width as i32, s.display_info.height as i32);
            cx >= sx && cx < sx + sw && cy >= sy && cy < sy + sh
        })
        .or_else(|| {
            screens.iter().find(|s| {
                let (sx, sy) = (s.display_info.x, s.display_info.y);
                let (sw, sh) = (s.display_info.width as i32, s.display_info.height as i32);
                x >= sx && x < sx + sw && y >= sy && y < sy + sh
            })
        })
        .unwrap_or(&screens[0]);

    let scale = screen.display_info.scale_factor.max(0.01);
    // 绝对物理坐标 -> 相对该显示器原点的逻辑坐标
    let rel_x = ((x - screen.display_info.x) as f32 / scale).round() as i32;
    let rel_y = ((y - screen.display_info.y) as f32 / scale).round() as i32;
    let rel_w = (w as f32 / scale).round() as u32;
    let rel_h = (h as f32 / scale).round() as u32;

    let image = screen
        .capture_area(rel_x, rel_y, rel_w.max(1), rel_h.max(1))
        .ok()?;
    let (w, h) = image.dimensions();
    Some(RgbaFrame {
        rgba: image.into_raw(),
        w,
        h,
    })
}

/// 截取虚拟桌面指定区域（绝对物理像素），失败返回 None。
/// RGBA→RGB 采用就地压缩：目标步长 3 ≤ 源步长 4，前向写入安全，
/// 避免每帧再分配一块全屏缓冲。
pub fn capture_region(x: i32, y: i32, w: i32, h: i32) -> Option<Frame> {
    let RgbaFrame { mut rgba, w, h } = capture_region_rgba(x, y, w, h)?;
    let n = (w as usize) * (h as usize);
    for i in 0..n {
        let s = i * 4;
        let d = i * 3;
        rgba[d] = rgba[s];
        rgba[d + 1] = rgba[s + 1];
        rgba[d + 2] = rgba[s + 2];
    }
    rgba.truncate(n * 3);
    Some(Frame { rgb: rgba, w, h })
}

/// 截取所有显示器的完整画面（全屏找码用），按显示器顺序返回。
/// 所有显示器的虚拟桌面矩形（x/y/w/h 物理像素），带 1 秒缓存
/// （扫码循环每 150ms 枚举一次；热拔插显示器后最多 1 秒内生效）。
pub fn screen_rects() -> Vec<(i32, i32, i32, i32)> {
    use std::sync::{Mutex, OnceLock, PoisonError};
    use std::time::{Duration, Instant};
    type Rect = (i32, i32, i32, i32);
    static CACHE: OnceLock<Mutex<(Instant, Vec<Rect>)>> = OnceLock::new();
    let cache =
        CACHE.get_or_init(|| Mutex::new((Instant::now() - Duration::from_secs(2), Vec::new())));
    let mut c = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if c.0.elapsed() < Duration::from_secs(1) {
        return c.1.clone();
    }
    let rects: Vec<(i32, i32, i32, i32)> = screenshots::Screen::all()
        .map(|screens| {
            screens
                .iter()
                .map(|s| {
                    let d = s.display_info;
                    (d.x, d.y, d.width as i32, d.height as i32)
                })
                .collect()
        })
        .unwrap_or_default();
    c.0 = Instant::now();
    c.1 = rects.clone();
    rects
}
