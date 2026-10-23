use std::sync::Arc;

/// AVCC（4 字节长度前缀）→ Annex-B（00 00 00 01 起始码）。
pub fn avcc_to_annexb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let n = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if n == 0 || i + n > data.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[i..i + n]);
        i += n;
    }
    out
}

/// AVCDecoderConfigurationRecord（FLV sequence header 体）→ SPS/PPS Annex-B。
pub fn avcc_config_to_annexb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if data.len() < 7 {
        return out;
    }
    let mut i = 5usize;
    let sps_count = data[i] & 0x1F;
    i += 1;
    let read_nalus = |count: usize, data: &[u8], i: &mut usize, out: &mut Vec<u8>| {
        for _ in 0..count {
            if *i + 2 > data.len() {
                return;
            }
            let n = u16::from_be_bytes([data[*i], data[*i + 1]]) as usize;
            *i += 2;
            if n == 0 || *i + n > data.len() {
                return;
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&data[*i..*i + n]);
            *i += n;
        }
    };
    read_nalus(sps_count as usize, data, &mut i, &mut out);
    if i < data.len() {
        let pps_count = data[i] & 0x1F;
        i += 1;
        read_nalus(pps_count as usize, data, &mut i, &mut out);
    }
    out
}

/// 紧凑 YUV420 裁剪区 → 灰度（1 字节/像素），面积平均降采样到 tw×th（扫码用）。
///
/// **扫码只需亮度**：直接取 Y（编码 luma）输出灰度，跳过「YUV→RGB→灰度」两步
/// 与 RGB 中间缓冲（1080p 每帧少写 2/3 字节、少一次全图加权遍历）。
/// 与 `qr` 侧 BT.601 加权灰度相比仅差一个全局线性亮度变换（Y 为有限范围、
/// 权重近似全范围），二维码二值化对全局线性变换不敏感，识别行为不变。
/// 箱映射与既有实现一致（`sx0 = cx + ox*cw/tw`），step 为采样步长（热路径 1）。
/// `src_w` 为源帧宽（紧凑布局，Y 跨距 = src_w）。
#[allow(clippy::too_many_arguments)]
pub(super) fn yuv_crop_to_gray(
    y: &[u8],
    src_w: usize,
    src_h: usize,
    crop: (u32, u32, u32, u32),
    tw: usize,
    th: usize,
    step: usize,
) -> Vec<u8> {
    let (cx, cy, cw, ch) = (
        crop.0 as usize,
        crop.1 as usize,
        crop.2 as usize,
        crop.3 as usize,
    );
    let cw = cw.min(src_w.saturating_sub(cx)).max(1);
    let ch = ch.min(src_h.saturating_sub(cy)).max(1);
    let (tw, th) = (tw.max(1), th.max(1));
    let step = step.max(1);
    let mut out = vec![0u8; tw * th];
    // 快路径：1:1 无缩放（扫码常态——输出尺寸上限 1920 ≥ 直播流宽，不缩放）且不抽样
    // → 逐行 memcpy。原实现对每个输出像素做一次整数除法（`ysum / yn`），1080p 达 200 万
    // 次除法、约 10ms+；这是扫描链路上灰度转换耗时的主因。
    if tw == cw && th == ch && step == 1 {
        for row in 0..ch {
            let src0 = (cy + row) * src_w + cx;
            out[row * cw..(row + 1) * cw].copy_from_slice(&y[src0..src0 + cw]);
        }
        return out;
    }
    // 步长采样快路径：整数倍降采样（step 恰好覆盖箱窗口 → 每输出像素取窗口首点）。
    // 常规分支对每个输出像素做一次整数除法（`ysum / yn`），720p 的 1/2 降采样即
    // 23 万次除法 ≈ 2ms —— 实测占主档取帧耗时的 95%。本分支无除法、无内层平均
    // 循环；且**步长采样对 QR 的检出优于箱平均**（箱平均抹平模块边缘对比度，
    // 而 HybridBinarizer 依赖局部对比：90px 码箱平均 0/8、步长 8/8，
    // 见 tests/bench_qr_pyramid.rs）。
    if step > 1 && tw * step == cw && th * step == ch {
        for oy in 0..th {
            let sy = cy + oy * step;
            let src = &y[sy * src_w + cx..sy * src_w + cx + cw];
            let dst = &mut out[oy * tw..(oy + 1) * tw];
            for ox in 0..tw {
                dst[ox] = src[ox * step];
            }
        }
        return out;
    }
    // 预计算行/列映射（避免内层重复除法）。
    let mut row_ys: Vec<(usize, usize)> = Vec::with_capacity(th);
    for oy in 0..th {
        let sy0 = cy + oy * ch / th;
        let sy1 = (cy + (oy + 1) * ch / th).max(sy0 + 1);
        row_ys.push((sy0, sy1));
    }
    let mut col_xs: Vec<(usize, usize)> = Vec::with_capacity(tw);
    for ox in 0..tw {
        let sx0 = cx + ox * cw / tw;
        let sx1 = (cx + (ox + 1) * cw / tw).max(sx0 + 1);
        col_xs.push((sx0, sx1));
    }
    let mut oi = 0usize;
    for &(sy0, sy1) in row_ys.iter() {
        for &(sx0, sx1) in col_xs.iter() {
            let mut ysum = 0u32;
            let mut yn = 0u32;
            let mut yy = sy0;
            while yy < sy1 {
                let base = yy * src_w;
                let mut xx = sx0;
                while xx < sx1 {
                    ysum += y[base + xx] as u32;
                    yn += 1;
                    xx += step;
                }
                yy += step;
            }
            out[oi] = (ysum / yn.max(1)) as u8;
            oi += 1;
        }
    }
    out
}

/// 预览专用：YUV420 → RGBA 最近邻采样（O(tw×th)，无箱平均内层循环）。
/// 预览仅用于人眼观察，最近邻足够；比箱平均快 5~10 倍（实测转换是 CPU 大头）。
/// 输出尺寸按 tw×th，源像素取映射点（不做平均）。
#[allow(clippy::too_many_arguments)]
pub(super) fn yuv_crop_to_rgba(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    src_w: usize,
    src_h: usize,
    crop: (u32, u32, u32, u32),
    tw: usize,
    th: usize,
    _step: usize,
) -> Vec<u8> {
    let (cx, cy, cw, ch) = (
        crop.0 as usize,
        crop.1 as usize,
        crop.2 as usize,
        crop.3 as usize,
    );
    let cw = cw.min(src_w.saturating_sub(cx)).max(1);
    let ch = ch.min(src_h.saturating_sub(cy)).max(1);
    let (tw, th) = (tw.max(1), th.max(1));
    let cw2 = src_w / 2;
    let mut out = vec![0u8; tw * th * 4];
    // 预计算每列的 Y 索引与色度索引（避免内层重复乘除）。
    let mut ycol = vec![0usize; tw];
    let mut ccol = vec![0usize; tw];
    let cmax = cw2.saturating_sub(1);
    for ox in 0..tw {
        let sx = (cx + ox * cw / tw).min(src_w - 1);
        ycol[ox] = sx;
        ccol[ox] = (sx / 2).min(cmax);
    }
    let hmax = src_h.saturating_sub(1);
    // 安全前提：cy+ch≤src_h、ycol/ccol 已夹取、sy≤hmax、cyy≤src_h/2-1。
    // 用 get_unchecked 去掉热路径边界检查（本函数是 CPU 大头，实测显著）。
    unsafe {
        let yp = y.as_ptr();
        let up = u.as_ptr();
        let vp = v.as_ptr();
        let op = out.as_mut_ptr();
        let mut oi = 0usize;
        for oy in 0..th {
            let sy = (cy + oy * ch / th).min(hmax);
            let cyy = (sy / 2).min(src_h / 2 - 1);
            let ybase = sy * src_w;
            let cbase = cyy * cw2;
            for ox in 0..tw {
                let ysx = *ycol.get_unchecked(ox);
                let csx = *ccol.get_unchecked(ox);
                let yy = *yp.add(ybase + ysx) as i32 - 16;
                let uu = *up.add(cbase + csx) as i32 - 128;
                let vv = *vp.add(cbase + csx) as i32 - 128;
                *op.add(oi) = ((1192 * yy + 1634 * vv) >> 10).clamp(0, 255) as u8;
                *op.add(oi + 1) = ((1192 * yy - 400 * uu - 833 * vv) >> 10).clamp(0, 255) as u8;
                *op.add(oi + 2) = ((1192 * yy + 2066 * uu) >> 10).clamp(0, 255) as u8;
                *op.add(oi + 3) = 255;
                oi += 4;
            }
        }
    }
    out
}

// ---------- 会话与帧总线 ----------

/// 解码产出的一帧（紧凑 YUV420 / I420）。
///
/// **为什么存 YUV 而不是 RGBA**：解码器原生输出即 YUV420（1.5 字节/像素），
/// 早先每帧全帧转 RGBA（4 字节/像素）纯属浪费——720p 每帧要转换 92 万像素，
/// 30fps 下每秒 2760 万像素；而预览只输出 ~320 宽、扫码只取 ROI。
/// 现改存紧凑 YUV（内存与拷贝量降 62%），把 YUV→RGB 转换**推迟到消费端、
/// 且只转换输出像素所需的源区域**（转换量降一个数量级）。
pub(super) struct Frame {
    pub(super) seq: u64,
    /// FLV 媒体时间戳（ms，流自身时钟）。输出侧追帧与诊断用。
    pub(super) ts: u32,
    pub(super) w: u32,
    pub(super) h: u32,
    /// Y 平面，紧凑布局（行跨距 = w）。
    pub(super) y: Vec<u8>,
    /// U 平面（w/2 × h/2，行跨距 = w/2）。
    pub(super) u: Vec<u8>,
    /// V 平面（w/2 × h/2，行跨距 = w/2）。
    pub(super) v: Vec<u8>,
}

/// 帧总线：小型回放缓冲（FIFO）+ 独立的最新帧。
///
/// **两条消费路径必须解耦**：
/// - 显示路径（crop_frame，前端 30fps 轮询）破坏性弹出队列最旧帧做节拍控制；
/// - 扫码路径（take_scan_frame，约 400ms 一次）只关心「当前最新画面」。
///
/// 早期设计让扫码也读同一队列的 back()：低帧率流（720p ~10fps）下显示弹出(30fps)
/// 远快于到达(10fps)，队列常被榨空 → 扫码永远取不到帧（实测 0%）。现在解码线程
/// 同时维护 latest（Arc 共享，零拷贝），扫码只读它，与显示解耦。
pub(super) struct Frames {
    pub(super) queue: std::collections::VecDeque<Arc<Frame>>,
    /// 最新一帧（解码线程每次产出即更新；扫码专用，不受显示弹出影响）。
    pub(super) latest: Option<Arc<Frame>>,
    /// 上次扫码输出的 RGB 缓存。无新帧时复用，保证扫码供帧率 100%——
    /// 低帧率流（如 ~2fps）不会因某轮 400ms 窗口内无新帧而「停一拍」
    /// （实测 100x178 流会丢 15%~23% 的采样轮次）。
    pub(super) scan_cache: Option<ScanCache>,
    /// 单调递增的帧序号。**不能**用「队列尾 seq + 1」——队列空时那会导致
    /// 新帧 seq 恒为 1，使 LAST_SENT 去重把不同帧误判为同一帧（预览卡成 1 帧）。
    pub(super) next_seq: u64,
}

/// 队列硬上限（≈400ms@30fps；防极端突发撑爆内存，超出丢弃最旧帧）。
///
/// 2026-10-11 曾由 6 降到 4（当时想省 ~33~67ms 延迟）。2026-10-22 实测发现
/// 这个深度**不足以吸收 CDN 的突发投递**：同一会话里解码侧稳定 27~30 帧/秒、
/// 前端也在 52 次/秒地取帧，但队列一到 5 帧就丢最旧帧 → 实测「取帧 52/s、
/// 新帧只有 17/s、队列常满」。丢的是**本可以播出的帧**，画面因此发顿。
///
/// 现在前面还有一层前端抖动缓冲（display_delay_ms，默认 300ms）负责播出节拍，
/// 这层队列只需「不丢帧」：消费端（前端 30~60 次/秒）快于生产端（27~30 帧/秒），
/// 稳态下队列长期接近空，加深**不会**增加稳态延迟，只在突发时不再丢帧。
pub(super) const FRAME_BUFFER_MAX: usize = 12;

/// 允许跳过 disposable 帧（frame_type=3）的落后量阈值（毫秒）。
///
/// 这类帧不影响后续解码的参考链，跳过可省一半解码量；但带时间分层的流里它可能
/// 占到一半的帧——无条件跳过等于把 30fps 砍成 15fps（2026-10-22 复核：这是
/// 「画面只有十几帧」的另一条可能成因）。因此只在**确实落后**（解码跟不上、
/// socket 在积压）时才跳，正常状态全帧解码。
pub(super) const DISPOSABLE_SKIP_LAG_MS: i64 = 150;

/// 直播帧统计（进程级累计，诊断用；Relaxed 原子，逐帧 3~5 次自增，成本可忽略）。
///
/// 用途：回答「解码到底出没出帧、有多少帧被主动丢弃」这类只能实测的问题——
/// 出帧率、主动丢弃、解码调用/产出比都必须有真值，否则只能靠猜。
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// 视频 tag 总数（含非 AVC 与不可解码的）。
    pub video_tags: u64,
    /// H.264 关键帧（frame_type=1）数。
    pub key: u64,
    /// 普通参考帧（frame_type=2）数。
    pub inter: u64,
    /// 可丢弃参考帧（frame_type=3，时间分层）数。
    pub disposable: u64,
    /// 其中被本实现主动跳过（未送解码器）的数量。
    pub disposable_skipped: u64,
    /// 送入解码器的访问单元数。
    pub decode_calls: u64,
    /// 解码器实际产出的帧数（一次调用可能产出多帧）。
    pub decode_out: u64,
    /// 预览取帧调用数（前端 capture_live_frame）。
    pub preview_calls: u64,
    /// 其中真正拿到新帧的调用数。
    pub preview_frame: u64,
    /// 其中「队列空 / 帧未变」的调用数（欠载或去重）。
    pub preview_empty: u64,
    /// 其中「会话不存在」的调用数。
    pub preview_nosession: u64,
}

pub(super) mod stats {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static VIDEO_TAGS: AtomicU64 = AtomicU64::new(0);
    pub static KEY: AtomicU64 = AtomicU64::new(0);
    pub static INTER: AtomicU64 = AtomicU64::new(0);
    pub static DISPOSABLE: AtomicU64 = AtomicU64::new(0);
    pub static DISPOSABLE_SKIPPED: AtomicU64 = AtomicU64::new(0);
    pub static DECODE_CALLS: AtomicU64 = AtomicU64::new(0);
    pub static DECODE_OUT: AtomicU64 = AtomicU64::new(0);
    pub static PREVIEW_CALLS: AtomicU64 = AtomicU64::new(0);
    pub static PREVIEW_FRAME: AtomicU64 = AtomicU64::new(0);
    pub static PREVIEW_EMPTY: AtomicU64 = AtomicU64::new(0);
    pub static PREVIEW_NOSESSION: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn bump(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }
    pub(crate) fn get(c: &AtomicU64) -> u64 {
        c.load(Ordering::Relaxed)
    }
}

/// 读取当前进程的直播帧统计（诊断/探针用）。
pub fn frame_stats() -> FrameStats {
    FrameStats {
        video_tags: stats::get(&stats::VIDEO_TAGS),
        key: stats::get(&stats::KEY),
        inter: stats::get(&stats::INTER),
        disposable: stats::get(&stats::DISPOSABLE),
        disposable_skipped: stats::get(&stats::DISPOSABLE_SKIPPED),
        decode_calls: stats::get(&stats::DECODE_CALLS),
        decode_out: stats::get(&stats::DECODE_OUT),
        preview_calls: stats::get(&stats::PREVIEW_CALLS),
        preview_frame: stats::get(&stats::PREVIEW_FRAME),
        preview_empty: stats::get(&stats::PREVIEW_EMPTY),
        preview_nosession: stats::get(&stats::PREVIEW_NOSESSION),
    }
}

/// 清零直播帧统计（探针在每轮测量前调用）。
pub fn reset_frame_stats() {
    for c in [
        &stats::VIDEO_TAGS,
        &stats::KEY,
        &stats::INTER,
        &stats::DISPOSABLE,
        &stats::DISPOSABLE_SKIPPED,
        &stats::DECODE_CALLS,
        &stats::DECODE_OUT,
    ] {
        c.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 扫码灰度缓存：键为 (帧序号, 裁剪区)，视口不变且无新帧时直接复用。
/// gray 用 Arc：命中时只克隆指针（零拷贝），避免在持锁路径上复制整块图像。
pub(super) struct ScanCache {
    pub(super) seq: u64,
    pub(super) crop: Option<(u32, u32, u32, u32)>,
    pub(super) gray: Arc<Vec<u8>>,
    pub(super) w: u32,
    pub(super) h: u32,
    /// 缓存帧的降采样倍率：主档（2）与兜底档（1）交替取帧时，倍率不同的缓存
    /// 不能复用（尺寸不同），否则会把半分辨率帧当成全分辨率帧解码。
    pub(super) scale: u32,
}

/// 上次发送的预览帧标识：(seq, 裁剪区, 输出宽)。相同则返回短响应。
pub(super) type SentKey = (u64, (u32, u32, u32, u32), u32);

/// 扫码取帧输出的最长边上限（像素）：超过则等比缩小以控制 YUV→RGB 成本；
/// 1080p 及以下流不受影响（原分辨率输出）。
pub(super) const SCAN_MAX_LONG_SIDE: u32 = 1920;

/// 扫码取帧的输出尺寸：裁剪区不超过上限时原分辨率输出（保二维码模块细节），
/// 超过则等比缩小到最长边 = SCAN_MAX_LONG_SIDE。
/// 直播扫码主档的降采样倍率（相对源帧，2 = 半分辨率）。
///
/// 依据（2026-10-13 基准 `tests/bench_qr_pyramid.rs`，24 帧真实 720p 直播画面
/// + 带静区合成码，含「模糊×1 / 模糊×2+噪声」两级降质）：
/// - 耗时：主档 1/2 步长 **2.45ms/帧**（含降采样），全分辨率 5.50ms/帧（-55%）；
///   叠加 800ms 节流的全分辨率兜底后均值 2.39ms/帧 ≈ **6.0% 核**（旧 17.5%）。
/// - 检出：≥120px 的码在最强降质下仍 **8/8 全中**（与全分辨率持平）；
///   90px 降质小码主档漏检 → 由周期性全分辨率兜底覆盖（延迟 ≤800ms）；
///   70px 极限小码全分辨率同样检不出，非本次回归。
/// - 采样方式：步长（最近邻）优于箱平均——后者抹平模块边缘对比度，
///   HybridBinarizer 依赖局部对比，实测 90px 码箱平均 0/8、步长 8/8。
///
/// 屏幕（桌面复制）路径仍为 1：截图清晰、且有 400ms 节流与 ROI 加速兜底，
/// 不以识别率换 CPU。
pub const LIVE_SCAN_SCALE: u32 = 2;

/// 扫码帧输出尺寸：裁剪区按 `SCAN_MAX_LONG_SIDE` 上限等比缩小（保模块细节）。
pub fn scan_output_size(cw: u32, ch: u32) -> (u32, u32) {
    let (cw, ch) = (cw.max(1), ch.max(1));
    if cw.max(ch) <= SCAN_MAX_LONG_SIDE {
        return (cw, ch);
    }
    let lim = SCAN_MAX_LONG_SIDE as u64;
    if cw >= ch {
        (
            SCAN_MAX_LONG_SIDE,
            ((ch as u64 * lim) / cw as u64).max(1) as u32,
        )
    } else {
        (
            ((cw as u64 * lim) / ch as u64).max(1) as u32,
            SCAN_MAX_LONG_SIDE,
        )
    }
}

/// 扫码帧的内容标识：`(帧序号, 裁剪区)`。相同表示像素完全未变。
pub type ScanKey = (u64, Option<(u32, u32, u32, u32)>);

/// 扫码用的一帧：灰度像素 + 变化标识 + 降采样倍率。
pub struct ScanFrame {
    /// 灰度像素（1 字节/像素，紧凑布局：行跨距 = w）。Arc 共享：命中缓存时
    /// 只克隆指针（零拷贝），避免每轮 60Hz 轮询复制整帧（1080p ~2MB）。
    pub gray: Arc<Vec<u8>>,
    pub w: u32,
    pub h: u32,
    /// 内容标识：与上轮相同表示像素完全未变（缓存命中）。扫码侧据此跳过对
    /// 同一帧的重复解码（流帧率低于轮询频率时的纯浪费）。
    pub key: ScanKey,
    /// `gray` 相对源帧的降采样倍率（1 = 原分辨率；2 = 1/2 步长采样）。
    ///
    /// 直播扫码主档取 2（见 `LIVE_SCAN_SCALE`）：解码成本 ∝ 像素数，1/2 降采样
    /// 把 720p 主扫描从 5.5ms 压到 2.45ms/帧（-55%）。
    ///
    /// 实测（24 帧真实直播画面 + 带静区合成码，含模糊/噪声降质）≥120px 的码
    /// 检出率与全分辨率完全持平（8/8），90px 及以下的降质小码由周期性
    /// 全分辨率兜底覆盖。
    pub scale: u32,
}
