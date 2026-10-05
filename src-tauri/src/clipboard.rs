//! 系统剪贴板：读取位图（CF_DIB/CF_DIBV5）与清空。
//!
//! 替代 arboard 依赖（其携带完整 image 库）；只实现本工具需要的两个操作，
//! 坐标/行序按 Windows DIB 规范手工解析。

use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;

/// 清空剪贴板（失败静默——剪贴板被其它进程占用时无可操作空间）。
pub fn clear() {
    unsafe {
        if OpenClipboard(None).is_ok() {
            let _ = EmptyClipboard();
            let _ = CloseClipboard();
        }
    }
}

/// 读取剪贴板位图，返回 (RGBA, 宽, 高)（自上而下）；非位图或解析失败返回 None。
pub fn read_image_rgba() -> Option<(Vec<u8>, u32, u32)> {
    unsafe {
        OpenClipboard(None).ok()?;
        let out = (|| {
            let handle = match GetClipboardData(CF_DIB) {
                Ok(h) => h,
                Err(_) => GetClipboardData(CF_DIBV5).ok()?,
            };
            if handle.is_invalid() {
                return None;
            }
            let hglobal = HGLOBAL(handle.0);
            let size = GlobalSize(hglobal);
            let ptr = GlobalLock(hglobal) as *const u8;
            if ptr.is_null() {
                return None;
            }
            let parsed = parse_dib(std::slice::from_raw_parts(ptr, size));
            let _ = GlobalUnlock(hglobal);
            parsed
        })();
        let _ = CloseClipboard();
        out
    }
}

/// 解析 DIB 内存块（BITMAPINFOHEADER / BITMAPV5HEADER，24 或 32bpp，BI_RGB /
/// BI_BITFIELDS），返回自上而下的 RGBA。
fn parse_dib(b: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    if b.len() < 40 {
        return None;
    }
    let u32_at =
        |o: usize| -> Option<u32> { Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?)) };
    let i32_at =
        |o: usize| -> Option<i32> { Some(i32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?)) };
    let u16_at =
        |o: usize| -> Option<u16> { Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?)) };

    let header = u32_at(0)? as usize;
    if !(40..=b.len()).contains(&header) {
        return None;
    }
    let width = i32_at(4)?;
    let height = i32_at(8)?;
    let bit_count = u16_at(14)? as usize;
    let compression = u32_at(16)?;
    let clr_used = u32_at(32)? as usize;
    if width <= 0 || height == 0 || !(bit_count == 24 || bit_count == 32) {
        return None;
    }
    let top_down = height < 0;
    let height = height.unsigned_abs() as usize;
    let width = width as usize;

    // 像素数据起点：头之后可能有颜色表 / BITFIELDS 掩码（仅 40 字节基本头时掩码在头后）。
    let mut offset = header;
    if compression == 3 && header == 40 {
        offset += 12;
    }
    if compression == 0 && bit_count <= 8 {
        offset += if clr_used > 0 {
            clr_used * 4
        } else {
            (1usize << bit_count) * 4
        };
    }

    let stride = (width * bit_count).div_ceil(32) * 4;
    if offset + stride * height > b.len() {
        return None;
    }
    let mut out = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let src_y = if top_down { y } else { height - 1 - y };
        let row = &b[offset + src_y * stride..][..width * (bit_count / 8)];
        if bit_count == 32 {
            for px in row.as_chunks::<4>().0 {
                // DIB 内存序 B,G,R,X → RGBA
                out.push(px[2]);
                out.push(px[1]);
                out.push(px[0]);
                out.push(255);
            }
        } else {
            for px in row.as_chunks::<3>().0 {
                out.push(px[2]);
                out.push(px[1]);
                out.push(px[0]);
                out.push(255);
            }
        }
    }
    Some((out, width as u32, height as u32))
}
