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

/// 一帧截图的 RGB 数据。
pub struct Frame {
    pub rgb: Vec<u8>,
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

/// 截取虚拟桌面指定区域（绝对物理像素），返回 RGB，失败返回 None。
/// 源 DC 用虚拟屏 DC（`CreateDCW("DISPLAY", null)`），BitBlt 源坐标即虚拟桌面绝对坐标，
/// 天然支持负坐标的多显示器布局。
pub fn capture_region(x: i32, y: i32, w: i32, h: i32) -> Option<Frame> {
    let w = w.max(1);
    let h = h.max(1);
    unsafe {
        let src = CreateDCW(w!("DISPLAY"), PCWSTR::null(), PCWSTR::null(), None);
        if src.is_invalid() {
            return None;
        }
        let frame = (|| {
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
                // GDI 32bpp 内存序为 B,G,R,X → 转为 RGB。
                let mut rgb = Vec::with_capacity(n * 3);
                for p in px.as_chunks::<4>().0 {
                    rgb.push(p[2]);
                    rgb.push(p[1]);
                    rgb.push(p[0]);
                }
                out = Some(Frame {
                    rgb,
                    w: w as u32,
                    h: h as u32,
                });
            }
            SelectObject(mem, old);
            let _ = DeleteObject(hbmp.into());
            let _ = DeleteDC(mem);
            out
        })();
        let _ = DeleteDC(src);
        frame
    }
}
