//! Android 摄像头捕获帧总线。
//!
//! 数据流：Kotlin（Camera2 + ImageReader YUV_420_888 回调线程）→ JNI `onFrame`
//! → [`push_frame`] 拷贝 **Y 平面**（剥离行 padding）→ 取帧侧拉取灰度帧。
//!
//! 与屏幕捕获（[`super::capture`]，RGBA）的差异：
//! - 相机原生输出 **YUV_420_888**，**扫码只需 Y（亮度）**——Y 平面即灰度，
//!   直接存储，跳过 YUV→RGB→灰度 两步（相机 Y 是唯一需要的平面）；
//! - 同样用「帧序号 + 采样指纹」做去重与变化检测。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// 最新相机帧（灰度：Y 平面紧凑存储，行跨距恒为 w）。
pub struct CameraFrame {
    pub gray: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// 单调递增帧序号。
    pub seq: u64,
    /// 采样指纹（内容变化检测）。
    pub fingerprint: u64,
}

static SLOT: OnceLock<Mutex<Option<CameraFrame>>> = OnceLock::new();
/// `scan_frame` 上次消费的帧序号。
static LAST_SCAN_SEQ: AtomicU64 = AtomicU64::new(0);
/// `preview_frame` 上次消费的帧序号。
static LAST_PREVIEW_SEQ: AtomicU64 = AtomicU64::new(0);

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

/// JNI 回调线程推入一帧相机 Y 平面（`row_stride` 可能含行尾对齐 padding）。
///
/// `src` 为 Y 平面缓冲；像素跨距恒 1（相机 Y 平面保证）。锁内逐行剥离 padding。
pub fn push_frame(src: &[u8], w: u32, h: u32, row_stride: usize) {
    if w == 0 || h == 0 || row_stride == 0 {
        return;
    }
    let rows = h as usize;
    let usable = (src.len() / row_stride).min(rows);
    if usable == 0 {
        return;
    }
    let ww = w as usize;
    let copy_w = ww.min(row_stride);

    let mut g = lock();
    let prev = g.take();
    let seq = prev.as_ref().map(|f| f.seq).unwrap_or(0) + 1;
    let mut data = prev.map(|f| f.gray).unwrap_or_default();
    data.resize(ww * rows, 0);
    for y in 0..usable {
        let off = y * row_stride;
        data[y * ww..y * ww + copy_w].copy_from_slice(&src[off..off + copy_w]);
    }
    let fp = fingerprint(&data);
    *g = Some(CameraFrame {
        gray: data,
        w,
        h,
        seq,
        fingerprint: fp,
    });
}

/// 相机会话停止（用户停止 / 出错 / 服务销毁）：清空帧槽与去重状态。
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
        (f.gray.clone(), f.w, f.h)
    } else {
        downsample(&f.gray, f.w, f.h, scale as usize)
    };
    Some(crate::screenshot::GrayFrame { gray, w, h })
}

/// 取一帧灰度用于预览（RGBA 输出：Y 复制到 R/G/B）。无新帧返回 Unchanged。
pub fn preview_frame(tw: u32) -> Option<(Vec<u8>, u32, u32)> {
    let g = lock();
    let f = g.as_ref()?;
    if f.seq == last_preview_seq() {
        return None;
    }
    set_last_preview_seq(f.seq);
    let ow = tw.clamp(1, f.w.max(1));
    let oh = ((f.h as u64 * ow as u64) / f.w as u64).max(1) as u32;
    let mut out = Vec::with_capacity(ow as usize * oh as usize * 4);
    for oy in 0..oh {
        let sy = (oy as usize * f.h as usize) / oh as usize;
        for ox in 0..ow {
            let sx = (ox as usize * f.w as usize) / ow as usize;
            let v = f.gray[sy * f.w as usize + sx];
            out.extend_from_slice(&[v, v, v, 255]);
        }
    }
    Some((out, ow, oh))
}

/// 步长降采样（整数倍，无除法内层）。
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
