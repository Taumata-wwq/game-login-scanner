//! 显示器枚举与区域截屏（Win32 GDI），输出 RGB 帧。
//!
//! 坐标系约定：传入的 x/y/w/h 为「虚拟桌面绝对坐标，物理像素」。进程为
//! Per-Monitor V2 DPI 感知（tao 初始化），GDI 在此模式下直接工作于物理像素。

use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDCW, CreateDIBSection, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetMonitorInfoW, SelectObject, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT,
    DIB_RGB_COLORS, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW, SRCCOPY,
};

/// 一帧截图的 RGB 数据（预览 GDI 兜底路径用；扫码路径用 `GrayFrame`）。
pub struct Frame {
    pub rgb: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// 一帧截图的灰度数据（1 字节/像素，紧凑布局：行跨距 = w）。
///
/// 扫码只依赖亮度信息，直接产出灰度可省去「RGB 三通道写入 + 二次 RGB→灰度
/// 遍历」的重复开销（GDI 兜底路径同样受益）。
pub struct GrayFrame {
    pub gray: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// 枚举所有显示器的（虚拟桌面矩形，设备名）。
fn monitors() -> Vec<(RECT, String)> {
    struct Collect(Vec<(RECT, String)>);
    unsafe extern "system" fn proc_(
        hmonitor: HMONITOR,
        _hdc: HDC,
        rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let list = &mut *(lparam.0 as *mut Collect);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            let name_len = info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szDevice.len());
            list.0
                .push((*rect, String::from_utf16_lossy(&info.szDevice[..name_len])));
        }
        true.into()
    }
    let mut out = Collect(Vec::new());
    let lparam = LPARAM(&mut out as *mut Collect as isize);
    unsafe {
        // 枚举失败时返回空（调用方报「未找到显示器」）。
        let _ = EnumDisplayMonitors(None, None, Some(proc_), lparam);
    }
    out.0
}

/// 所有显示器的虚拟桌面矩形（x/y/w/h 物理像素），带 1 秒缓存
/// （扫码循环每 150ms 枚举一次；热拔插显示器后最多 1 秒内生效）。
pub fn screen_rects() -> Vec<(i32, i32, i32, i32)> {
    type Rect = (i32, i32, i32, i32);
    static CACHE: OnceLock<Mutex<(Instant, Vec<Rect>)>> = OnceLock::new();
    let cache =
        CACHE.get_or_init(|| Mutex::new((Instant::now() - Duration::from_secs(2), Vec::new())));
    let mut c = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if c.0.elapsed() < Duration::from_secs(1) {
        return c.1.clone();
    }
    let rects: Vec<Rect> = monitors()
        .iter()
        .map(|(r, _)| (r.left, r.top, r.right - r.left, r.bottom - r.top))
        .collect();
    c.0 = Instant::now();
    c.1 = rects.clone();
    rects
}

/// 截取虚拟桌面指定区域并执行像素转换（RGB / 灰度共用 BitBlt 骨架）。
/// `convert` 拿到 BGRA 原始像素（自上而下，行跨距 = w*4）与像素数，返回转换结果。
fn capture_region_with<T>(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    convert: impl FnOnce(&[u8], u32, u32) -> T,
) -> Option<T> {
    let w = w.max(1);
    let h = h.max(1);
    unsafe {
        let src = CreateDCW(w!("DISPLAY"), PCWSTR::null(), PCWSTR::null(), None);
        if src.is_invalid() {
            return None;
        }
        let out = (|| {
            let mem = CreateCompatibleDC(Some(src));
            if mem.is_invalid() {
                return None;
            }
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h; // 自上而下
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = 0; // BI_RGB
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let hbmp =
                CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bits.is_null() {
                let _ = DeleteObject(hbmp.into());
                let _ = DeleteDC(mem);
                return None;
            }
            let old = SelectObject(mem, hbmp.into());
            let mut out = None;
            if BitBlt(mem, 0, 0, w, h, Some(src), x, y, SRCCOPY | CAPTUREBLT).is_ok() {
                let n = (w as usize) * (h as usize);
                let px = std::slice::from_raw_parts(bits as *const u8, n * 4);
                out = Some(convert(px, w as u32, h as u32));
            }
            SelectObject(mem, old);
            let _ = DeleteObject(hbmp.into());
            let _ = DeleteDC(mem);
            out
        })();
        let _ = DeleteDC(src);
        out
    }
}

/// 截取虚拟桌面指定区域（绝对物理像素），返回 RGB，失败返回 None。
/// 源 DC 用虚拟屏 DC（`CreateDCW("DISPLAY", null)`），BitBlt 源坐标即虚拟桌面绝对坐标，
/// 天然支持负坐标的多显示器布局。
pub fn capture_region(x: i32, y: i32, w: i32, h: i32) -> Option<Frame> {
    capture_region_with(x, y, w, h, |px, w, h| {
        // GDI 32bpp 内存序为 B,G,R,X → 转为 RGB。
        let mut rgb = Vec::with_capacity((w * h) as usize * 3);
        for p in px.as_chunks::<4>().0 {
            rgb.push(p[2]);
            rgb.push(p[1]);
            rgb.push(p[0]);
        }
        Frame { rgb, w, h }
    })
}

/// 截取虚拟桌面指定区域并直接转为灰度（扫码 GDI 兜底路径用）。
/// 灰度权重与 `qr` 侧 `(77R+151G+28B)>>8` 完全一致，跳过 RGB 中间缓冲。
pub fn capture_region_gray(x: i32, y: i32, w: i32, h: i32) -> Option<GrayFrame> {
    capture_region_with(x, y, w, h, |px, w, h| {
        let mut gray = Vec::with_capacity((w * h) as usize);
        for p in px.as_chunks::<4>().0 {
            // BGRA：p[2]=R, p[1]=G, p[0]=B。
            gray.push(((p[2] as u32 * 77 + p[1] as u32 * 151 + p[0] as u32 * 28) >> 8) as u8);
        }
        GrayFrame { gray, w, h }
    })
}
