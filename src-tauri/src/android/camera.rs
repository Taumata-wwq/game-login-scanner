//! Android 摄像头捕获帧总线。
//!
//! 数据流：Kotlin（Camera2 + ImageReader YUV_420_888 回调线程）→ JNI `onFrame`
//! → [`push_frame`] 拷贝 **YUV 三平面**（剥离行 padding）→ 取帧侧拉取：
//! - **扫码**：只用 Y（亮度）——即灰度，跳过 YUV→RGB→灰度；
//! **预览**：用 Y/U/V 做 YUV→RGB，输出**彩色**画面仅推 Y，预览为灰度）。
//!
//! 同样用「帧序号 + 采样指纹」做去重与变化检测。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// 最新相机帧（紧凑 YUV420：Y 行跨距 = w，U/V 行跨距 = w/2）。
pub struct CameraFrame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// 单调递增帧序号。
    pub seq: u64,
    /// 采样指纹（内容变化检测，抽在 Y 上）。
    pub fingerprint: u64,
}

static SLOT: OnceLock<Mutex<Option<CameraFrame>>> = OnceLock::new();
static LAST_SCAN_SEQ: AtomicU64 = AtomicU64::new(0);
static LAST_PREVIEW_SEQ: AtomicU64 = AtomicU64::new(0);
/// 相机 sensor 方向（度；预览旋转用）。
static ROTATION: AtomicU64 = AtomicU64::new(90);

/// 设置相机 sensor 方向（Kotlin 上报）。
pub fn set_rotation(deg: u32) {
    ROTATION.store(deg as u64, Ordering::Relaxed);
}

/// 相机 sensor 方向（度）。
pub fn rotation() -> u32 {
    ROTATION.load(Ordering::Relaxed) as u32
}

/// 取帧侧访问帧槽。
pub fn lock() -> MutexGuard<'static, Option<CameraFrame>> {
    SLOT.get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn fingerprint(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0usize;
    while i < data.len() {
        h ^= data[i] as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        i += 32;
    }
    h
}

/// 逐行拷贝一个平面（剥离行 padding）。pixel_stride=1 时整行拷贝；>1 时按跨距
/// 采样（相机 U/V 平面常 pixel_stride=2，与另一平面交错共享底层缓冲）。
fn copy_plane(
    dst: &mut [u8],
    src: &[u8],
    w: usize,
    h: usize,
    row_stride: usize,
    pixel_stride: usize,
) {
    let usable = (src.len() / row_stride.max(1)).min(h);
    if pixel_stride <= 1 {
        let copy_w = w.min(row_stride);
        for y in 0..usable {
            let off = y * row_stride;
            if off + copy_w > src.len() {
                break;
            }
            dst[y * w..y * w + copy_w].copy_from_slice(&src[off..off + copy_w]);
        }
    } else {
        for y in 0..usable {
            let roff = y * row_stride;
            for x in 0..w {
                let si = roff + x * pixel_stride;
                if si >= src.len() {
                    break;
                }
                dst[y * w + x] = src[si];
            }
        }
    }
}

/// JNI 回调线程推入一帧相机 YUV420（三平面，各含 row_stride padding）。
///
/// Y：像素跨距 1；U/V：像素跨距可能为 2（交错），尺寸 w/2 × h/2（紧凑存储）。
pub fn push_frame(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    w: u32,
    h: u32,
    y_row_stride: usize,
    uv_row_stride: usize,
    uv_pixel_stride: usize,
) {
    if w == 0 || h == 0 || y_row_stride == 0 || uv_row_stride == 0 {
        return;
    }
    let (ww, hh) = (w as usize, h as usize);
    let (cw, ch) = (ww / 2, hh / 2);

    let mut g = lock();
    let prev = g.take();
    let seq = prev.as_ref().map(|f| f.seq).unwrap_or(0) + 1;
    let mut yv = prev.as_ref().map(|f| f.y.clone()).unwrap_or_default();
    let mut uv = prev.as_ref().map(|f| f.u.clone()).unwrap_or_default();
    let mut vv = prev.as_ref().map(|f| f.v.clone()).unwrap_or_default();
    yv.resize(ww * hh, 0);
    uv.resize(cw * ch, 0);
    vv.resize(cw * ch, 0);
    copy_plane(&mut yv, y, ww, hh, y_row_stride, 1);
    copy_plane(&mut uv, u, cw, ch, uv_row_stride, uv_pixel_stride);
    copy_plane(&mut vv, v, cw, ch, uv_row_stride, uv_pixel_stride);
    drop(prev);

    let fp = fingerprint(&yv);
    *g = Some(CameraFrame {
        y: yv,
        u: uv,
        v: vv,
        w,
        h,
        seq,
        fingerprint: fp,
    });
}

/// 相机会话停止：清空帧槽与去重状态。
pub fn on_camera_stopped() {
    *lock() = None;
    LAST_SCAN_SEQ.store(0, Ordering::Relaxed);
    LAST_PREVIEW_SEQ.store(0, Ordering::Relaxed);
}

pub fn last_scan_seq() -> u64 {
    LAST_SCAN_SEQ.load(Ordering::Relaxed)
}
pub fn set_last_scan_seq(v: u64) {
    LAST_SCAN_SEQ.store(v, Ordering::Relaxed);
}
pub fn last_preview_seq() -> u64 {
    LAST_PREVIEW_SEQ.load(Ordering::Relaxed)
}
pub fn set_last_preview_seq(v: u64) {
    LAST_PREVIEW_SEQ.store(v, Ordering::Relaxed);
}

/// 取一帧灰度用于扫码：有新帧（序号变化）时返回，无则 None。
pub fn scan_frame(scale: u32) -> Option<crate::screenshot::GrayFrame> {
    let g = lock();
    let f = g.as_ref()?;
    if f.seq == last_scan_seq() {
        return None;
    }
    set_last_scan_seq(f.seq);
    let (gray, w, h) = if scale <= 1 {
        (f.y.clone(), f.w, f.h)
    } else {
        downsample(&f.y, f.w, f.h, scale as usize)
    };
    Some(crate::screenshot::GrayFrame { gray, w, h })
}

/// 取一帧彩色 RGBA 用于预览（YUV420→RGB，BT.601 有限范围）。无新帧返回 None。
pub fn preview_frame(tw: u32) -> Option<(Vec<u8>, u32, u32)> {
    let g = lock();
    let f = g.as_ref()?;
    if f.seq == last_preview_seq() {
        return None;
    }
    set_last_preview_seq(f.seq);
    let ow = tw.clamp(1, f.w.max(1));
    let oh = ((f.h as u64 * ow as u64) / f.w as u64).max(1) as u32;
    let (sw, sh) = (f.w as usize, f.h as usize);
    let cw = sw / 2;
    let mut out = Vec::with_capacity(ow as usize * oh as usize * 4);
    for oy in 0..oh {
        let sy = (oy as usize * sh) / oh as usize;
        for ox in 0..ow {
            let sx = (ox as usize * sw) / ow as usize;
            let yy = f.y[sy * sw + sx] as i32;
            let ci = (sy / 2) * cw + (sx / 2);
            let uu = f.u.get(ci).copied().unwrap_or(128) as i32 - 128;
            let vv = f.v.get(ci).copied().unwrap_or(128) as i32 - 128;
            // BT.601 有限范围 YUV→RGB。
            let c = (yy - 16).max(0);
            let d = uu;
            let e = vv;
            let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
            let gg = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
            out.extend_from_slice(&[r, gg, b, 255]);
        }
    }
    Some((out, ow, oh))
}

/// 步长降采样（整数倍）。
fn downsample(gray: &[u8], w: u32, h: u32, step: usize) -> (Vec<u8>, u32, u32) {
    let step = step.max(1);
    let ow = (w as usize / step).max(1);
    let oh = (h as usize / step).max(1);
    let mut out = vec![0u8; ow * oh];
    for oy in 0..oh {
        let sy = oy * step;
        if sy >= h as usize {
            break;
        }
        let row = &gray[sy * w as usize..];
        for ox in 0..ow {
            let sx = ox * step;
            if sx < w as usize {
                out[oy * ow + ox] = row[sx];
            }
        }
    }
    (out, ow as u32, oh as u32)
}
