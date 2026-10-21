//! Android 屏幕捕获帧总线。
//!
//! 数据流：Kotlin（MediaProjection + ImageReader 回调线程）→ JNI `onFrame`
//! → [`push_frame`] 拷贝为紧凑 RGBA（剥离行 padding）+ 采样指纹 → 取帧侧
//! （duplication / screenshot 门面）在锁内拉取并转换。
//!
//! 与 Windows `duplication.rs` 的模型差异：
//! - 推送模型（Android ImageReader，帧被动到达）vs 拉取模型（Windows DXGI）；
//! - 无脏矩形 / 瓦片机制：以「采样指纹 + 帧序号」替代变化检测与去重：
//!   `scan_frame` 用指纹判「内容是否有变化」，`preview_frame` 用序号判「是否有新帧」。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// 最新帧槽（RGBA_8888 紧凑存储：行跨距恒为 `w * 4`）。
pub struct FrameSlot {
    pub data: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// 单调递增帧序号（每次成功推入 +1）。
    pub seq: u64,
    /// 采样指纹（内容变化检测；见 [`fingerprint`]）。
    pub fingerprint: u64,
}

static SLOT: OnceLock<Mutex<Option<FrameSlot>>> = OnceLock::new();
/// 屏幕物理尺寸（授权建立时由 Kotlin 上报；供首帧到达前查询）。
static SCREEN: OnceLock<Mutex<Option<(u32, u32)>>> = OnceLock::new();
/// `scan_frame` 各降采样倍率上次消费的内容指纹（相同则视为画面无变化）。
///
/// **必须按 scale 分档**：主档（1/2）与兜底档（全分辨率）跑在同一份帧槽上，
/// 若共用一个指纹，主档先消费掉指纹后兜底档立刻被判「无变化」→ 全分辨率兜底
/// 永不执行（`scan.rs` 的 `FullSource::Screen` 沦为死代码）。与 Windows 侧
/// `MonitorDup::scan_seen[scale]` 的分档语义对齐。索引 0 不用（scale 从 1 起）。
static LAST_SCAN_FP: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
/// `preview_frame` 上次消费的帧序号。
static LAST_PREVIEW_SEQ: AtomicU64 = AtomicU64::new(0);

/// 取帧侧访问帧槽（锁内闭包内完成转换，避免大缓冲二次拷贝）。
pub fn lock() -> MutexGuard<'static, Option<FrameSlot>> {
    SLOT.get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// 采样指纹：每 32 字节取 1 字节做 FNV-1a。
///
/// 全帧逐字节哈希在 1080p 下约 5~10ms（30fps 不可接受）；采样后约 0.25ms。
/// 扫码场景的内容变化（二维码出现/消失）是大面积变化，采样足以捕捉。
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

/// JNI 回调线程推入一帧（RGBA_8888，`row_stride` 可能含行尾对齐 padding）。
///
/// 锁内完成「复用旧缓冲 → 逐行剥离 padding → 指纹 → 原子替换」；
/// 1080p 下持锁约 1~2ms（回调在专用 HandlerThread，不阻塞 UI）。
pub fn push_frame(src: &[u8], w: u32, h: u32, row_stride: usize) {
    if w == 0 || h == 0 || row_stride == 0 {
        return;
    }
    let rows = h as usize;
    // 可用行数不足（缓冲异常/被回收）时视为无效推入，保留现有帧。
    let usable = (src.len() / row_stride).min(rows);
    if usable == 0 {
        return;
    }

    let w4 = w as usize * 4;
    let copy_w = w4.min(row_stride);

    let mut g = lock();
    let prev = g.take();
    let seq = prev.as_ref().map(|f| f.seq).unwrap_or(0) + 1;
    let mut data = prev.map(|f| f.data).unwrap_or_default();
    data.resize(w4 * rows, 0);

    for y in 0..usable {
        let off = y * row_stride;
        data[y * w4..y * w4 + copy_w].copy_from_slice(&src[off..off + copy_w]);
    }
    // usable < rows 时底部余行保持 0（画面底部黑条，无害）。

    let fp = fingerprint(&data);
    *g = Some(FrameSlot {
        data,
        w,
        h,
        seq,
        fingerprint: fp,
    });
}

/// 上报屏幕物理尺寸（授权成功、捕获建立时调用）。
pub fn set_screen_size(w: u32, h: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let mut s = SCREEN
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    *s = Some((w, h));
}

/// 屏幕物理尺寸（未上报 / 未授权时为 None）。
pub fn screen_size() -> Option<(u32, u32)> {
    *SCREEN
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// 该 scale 档上次消费的指纹。`scale` 收敛到 `[1, 2]`（见 `rgba_gray_stride`）。
fn scan_fp_slot(scale: u32) -> &'static AtomicU64 {
    &LAST_SCAN_FP[scale.clamp(1, 2) as usize]
}

pub fn last_scan_fp(scale: u32) -> u64 {
    scan_fp_slot(scale).load(Ordering::Relaxed)
}
pub fn set_last_scan_fp(scale: u32, v: u64) {
    scan_fp_slot(scale).store(v, Ordering::Relaxed);
}
pub fn last_preview_seq() -> u64 {
    LAST_PREVIEW_SEQ.load(Ordering::Relaxed)
}
pub fn set_last_preview_seq(v: u64) {
    LAST_PREVIEW_SEQ.store(v, Ordering::Relaxed);
}

/// 捕获停止（系统状态栏停止共享 / 服务销毁）：清空帧槽与去重状态。
/// 屏幕尺寸保留（授权重建前展示层仍可能需要）。
pub fn on_capture_stopped() {
    *lock() = None;
    set_last_scan_fp(1, 0);
    set_last_scan_fp(2, 0);
    set_last_preview_seq(0);
}

// ---------- 转换原语 ----------

/// 把请求区域（屏幕坐标）换算为帧内合法裁剪矩形 `(x, y, w, h)`。
///
/// `region` 为扫描/预览请求区域（Android 单屏坐标系与帧同源）；
/// 越界/负宽度容错到帧内（至少 1x1）。
pub fn crop_of(region: (i32, i32, i32, i32), fw: u32, fh: u32) -> (usize, usize, u32, u32) {
    let x = region.0.clamp(0, fw as i32 - 1) as usize;
    let y = region.1.clamp(0, fh as i32 - 1) as usize;
    let w = (region.2.max(1) as usize).min(fw as usize - x).max(1) as u32;
    let h = (region.3.max(1) as usize).min(fh as usize - y).max(1) as u32;
    (x, y, w, h)
}

/// RGBA 裁剪 + 灰度 + 步长降采样（一趟完成）。
///
/// 与桌面端 `duplication::bgra_gray_stride` 对位（转换量同为 1/scale²）；
/// 通道序差异：RGBA_8888 为 `[i]=R, [i+1]=G, [i+2]=B`（桌面 BGRA 相反）。
/// 灰度权重与 `qr` 侧 `(77R + 151G + 28B) >> 8` 一致。
///
/// 调用方须保证 `(cx + cw, cy + ch)` 落在 `rgba` 覆盖的行内（越界会 panic）。
pub fn rgba_gray_stride(
    rgba: &[u8],
    pitch: usize,
    cx: usize,
    cy: usize,
    cw: u32,
    ch: u32,
    scale: usize,
) -> (Vec<u8>, u32, u32) {
    let scale = scale.clamp(1, 2);
    let ow = (cw as usize / scale).max(1);
    let oh = (ch as usize / scale).max(1);
    let mut out = vec![0u8; ow * oh];
    let mut oi = 0usize;
    for oy in 0..oh {
        let row = &rgba[(cy + oy * scale) * pitch + cx * 4..];
        for ox in 0..ow {
            // 按「输出列数」迭代并自乘步长（与桌面端同款防越界写法）。
            let i = ox * scale * 4;
            out[oi] = ((row[i] as u32 * 77 + row[i + 1] as u32 * 151 + row[i + 2] as u32 * 28) >> 8)
                as u8;
            oi += 1;
        }
    }
    (out, ow as u32, oh as u32)
}

/// RGBA 裁剪 + 最近邻降采样到目标宽度（禁止上采样），输出 RGBA。
///
/// 预览路径用；`want_w` 会被钳制到 `cw`（不放大，避免网格线问题）。
pub fn rgba_crop_scale(
    rgba: &[u8],
    pitch: usize,
    cx: usize,
    cy: usize,
    cw: u32,
    ch: u32,
    want_w: u32,
) -> (Vec<u8>, u32, u32) {
    let ow = want_w.clamp(1, cw.max(1));
    let oh = ((ch as u64 * ow as u64) / cw as u64).max(1) as u32;
    let mut out = Vec::with_capacity(ow as usize * oh as usize * 4);
    for oy in 0..oh {
        let sy = cy + (oy as usize * ch as usize) / oh as usize;
        let srow = &rgba[sy * pitch + cx * 4..];
        for ox in 0..ow {
            let sx = (ox as usize * cw as usize) / ow as usize;
            out.extend_from_slice(&srow[sx * 4..sx * 4 + 4]);
        }
    }
    (out, ow, oh)
}
