use std::sync::{Arc, PoisonError};

use super::frames::stats;
use super::frames::*;
use super::session::*;

/// 扫码消费者（多会话）：取指定会话的最新帧**当前显示视口**区域灰度。
/// 视口未知时用全帧。
///
/// `scale` = 降采样倍率（1 = 原分辨率，2 = 1/2）。直播主档传
/// `LIVE_SCAN_SCALE`（2）以压低逐帧解码成本；周期性兜底传 1 取全分辨率，
/// 覆盖降采样会漏掉的降质小码（见 `ScanFrame::scale` 的实测说明）。
pub fn take_scan_frame(id: &str, scale: u32) -> Option<ScanFrame> {
    let scale = scale.max(1);
    let s = get_session(id)?;
    let view = *s.last_view.lock().unwrap_or_else(PoisonError::into_inner);
    // 只在锁内取「帧引用 + 缓存命中判断」，**不在锁内做 YUV→灰度转换**：
    // 转换（原分辨率 1080p ~2M 像素）耗时远大于取引用，而解码线程 push 新帧
    // 需要同一把锁——持锁转换会把解码线程堵住 → 不读 socket → 延迟堆积，
    // 追帧也回不到低点。frame 是 Arc，clone 出来即可在锁外转换。
    let (frame, crop, cached) = {
        let f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
        // 读独立的 latest（不是显示队列）——显示弹出不影响扫码取帧。
        let frame = f.latest.clone()?;
        if frame.seq == 0 {
            return None;
        }
        let (fw, fh) = (frame.w, frame.h);
        // 裁剪区 = 视口（流坐标系），并夹到流尺寸内（此前用错坐标系且未夹到流边界，
        // 视口超出流时裁剪会越界）。
        let crop = view.and_then(|(x, y, w, h)| {
            if w <= 0 || h <= 0 {
                return None;
            }
            let x0 = (x.max(0) as u32).min(fw.saturating_sub(1));
            let y0 = (y.max(0) as u32).min(fh.saturating_sub(1));
            let cw0 = (w as u32).min(fw - x0).max(2);
            let ch0 = (h as u32).min(fh - y0).max(2);
            Some((x0, y0, cw0, ch0))
        });
        // 无新帧且缓存命中（视口未变）：复用上次灰度，保证供帧率 100%。
        let cached = f
            .scan_cache
            .as_ref()
            .filter(|c| c.seq == frame.seq && c.crop == crop && c.scale == scale)
            .map(|c| (c.gray.clone(), c.w, c.h));
        (frame, crop, cached)
    };
    let key = (frame.seq, crop);
    if let Some((gray, w, h)) = cached {
        return Some(ScanFrame {
            gray,
            w,
            h,
            key,
            scale,
        });
    }
    let (fw, fh) = (frame.w, frame.h);
    // 原分辨率输出（保二维码模块细节），仅超上限才等比缩小。
    // 此前无条件减半，导致全屏视口下二维码模块明显过小、无法识别（放大才可识别）。
    let crop_rect = crop.unwrap_or((0, 0, fw, fh));
    let (bw, bh) = scan_output_size(crop_rect.2, crop_rect.3);
    // 主档降采样：输出再缩 1/scale（直播常态 1/2）。
    let (ow, oh) = ((bw / scale).max(1), (bh / scale).max(1));
    // 采样步长 = 源/输出比：`yuv_crop_to_gray` 的箱窗口在 step 覆盖整个窗口时
    // 退化为「取窗口首点」= 步长采样。实测步长采样对 QR 的检出**优于**箱平均
    // （箱平均抹平模块边缘对比度，HybridBinarizer 依赖局部对比：90px 码箱平均
    // 0/8、步长 8/8），且成本更低。
    let step = ((crop_rect.2 / ow).max(1)) as usize;
    let gray = yuv_crop_to_gray(
        &frame.y,
        fw as usize,
        fh as usize,
        crop_rect,
        ow as usize,
        oh as usize,
        step,
    );
    let gray = Arc::new(gray);
    // 回写缓存（短锁）。
    {
        let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
        f.scan_cache = Some(ScanCache {
            seq: frame.seq,
            crop,
            gray: gray.clone(),
            w: ow,
            h: oh,
            scale,
        });
    }
    Some(ScanFrame {
        gray,
        w: ow,
        h: oh,
        key,
        scale,
    })
}

/// 预览消费者：按视口裁剪 + 面积降采样，组装 24 字节头 + RGBA 负载。
/// 头部布局与屏幕预览一致（mon = 流原始尺寸），无会话/无帧返回 None。
/// 预览帧输出语义。
/// - `NoSession`：会话不存在（未启动 / 已结束被移除）→ 前端自动切回屏幕；
/// - `Empty`：会话存活但缓冲欠载（流帧率低于显示节拍属正常现象）或帧未变
///   → 前端沿用上一帧；
/// - `Frame`：完整 24 字节头 + RGBA。
pub enum FrameOut {
    NoSession,
    Empty,
    Frame(Vec<u8>),
}

pub fn crop_frame(id: &str, x: i32, y: i32, w: i32, h: i32, max_width: u32) -> FrameOut {
    stats::bump(&stats::PREVIEW_CALLS, 1);
    let Some(s) = get_session(id) else {
        stats::bump(&stats::PREVIEW_NOSESSION, 1);
        return FrameOut::NoSession;
    };
    *s.last_view.lock().unwrap_or_else(PoisonError::into_inner) = if w > 0 && h > 0 {
        Some((x, y, w, h))
    } else {
        None
    };
    // 锁内只做：弹帧、计算裁剪/输出尺寸、克隆 Arc。YUV→RGBA 转换**在锁外**进行，
    // 避免长时间持有 frames 锁阻塞解码线程 push_frame（与 take_scan_frame 的
    // 「锁外转换」原则一致；此前 guard 被遮蔽未 drop，转换全程持锁）。
    let (frame, cx, cy, cw, ch, tw, th, step) = {
        let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
        if f.queue.is_empty() {
            stats::bump(&stats::PREVIEW_EMPTY, 1);
            return FrameOut::Empty;
        }
        // 弹出节拍 = 显示节拍（前端按设置的 30/60/120fps 轮询）：每轮询至多弹出一个
        // 最旧帧，队列空时复用队首帧。显示间隔由轮询时钟决定，到达抖动被缓冲吸收
        // ——「到达即弹出」会把抖动直接透传，缓冲再深也无济于事。
        let popped = f.queue.pop_front();
        // 上面已确认队列非空，pop_front 必返回 Some；or_else 兜底消除原 unwrap 的不可达分支（P1-9）。
        let Some(fref) = popped.as_ref().or_else(|| f.queue.front()) else {
            return FrameOut::Empty;
        };
        if fref.w == 0 || fref.h == 0 {
            return FrameOut::Empty;
        }
        let frame = fref.clone();
        let (fw, fh) = (frame.w as i32, frame.h as i32);
        let (cx, cy, cw, ch) = if w > 0 && h > 0 {
            let x0 = x.clamp(0, fw - 1);
            let y0 = y.clamp(0, fh - 1);
            let cw0 = (w.min(fw - x0)).max(1) as u32;
            let ch0 = (h.min(fh - y0)).max(1) as u32;
            (x0 as u32, y0 as u32, cw0, ch0)
        } else {
            (0, 0, frame.w, frame.h)
        };
        // 输出宽度不超过裁剪区实际像素数（禁上采样，避免黑网格）。
        let tw = max_width.min(cw).clamp(1, 4096);
        let th = (((ch as u64) * tw as u64) / cw as u64).max(1) as u32;
        // 预览允许采样：步长与旧 downsample_rgba 一致（cv/tw 后再折半），
        // 保证预览转换量约为全帧的 1/4~1/16。
        let step = (cw.div_ceil(tw).max(1).div_ceil(2).max(1)) as usize;
        (frame, cx, cy, cw, ch, tw, th, step)
    };
    // 帧未变（seq 相同）且视口/宽度未变：24 字节短响应，前端沿用上一帧
    // （复用屏幕预览协议），消除重复帧的降采样与 IPC。
    let key = (frame.seq, (cx, cy, cw, ch), tw);
    {
        let mut last = s.last_sent.lock().unwrap_or_else(PoisonError::into_inner);
        if *last == Some(key) {
            stats::bump(&stats::PREVIEW_EMPTY, 1);
            return FrameOut::Empty;
        }
        *last = Some(key);
        stats::bump(&stats::PREVIEW_FRAME, 1);
    }
    // 锁外转换：只转换输出像素所需的源区域。
    let rgba = yuv_crop_to_rgba(
        &frame.y,
        &frame.u,
        &frame.v,
        frame.w as usize,
        frame.h as usize,
        (cx, cy, cw, ch),
        tw as usize,
        th as usize,
        step,
    );
    let mut out = Vec::with_capacity(24 + rgba.len());
    out.extend_from_slice(&tw.to_le_bytes());
    out.extend_from_slice(&th.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&frame.w.to_le_bytes());
    out.extend_from_slice(&frame.h.to_le_bytes());
    out.extend_from_slice(&rgba);
    FrameOut::Frame(out)
}

// ---------- 解码线程 ----------
