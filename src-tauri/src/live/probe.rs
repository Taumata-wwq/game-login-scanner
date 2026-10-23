use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::bili::{resolve_source_sync, resolve_stream_sync, Platform};
use super::decode::decode_session;
use super::frames::*;
use super::hls_decode::decode_hls_session;

/// 单直播间冒烟测试结果。
#[derive(Debug, Clone)]
pub struct ProbeRoomResult {
    /// 是否 HLS-only 房间（false = FLV 流）。
    pub is_hls: bool,
    /// 完整成功（无错误信息且出帧 ≥ 1）。
    pub ok: bool,
    /// 出错信息（「未开播 / 解码失败 / 流传输过慢」等分类文本）。
    pub error: Option<String>,
    /// 出帧数。
    pub frames: u32,
    /// 出帧率（帧/秒，按测试时长计）。
    pub fps: f64,
    /// 输出分辨率（0×0 = 无帧）。
    pub w: u32,
    pub h: u32,
    /// 总耗时（毫秒，含解析 + 连接 + 运行 + 停止收尾）。
    pub elapsed_ms: u64,
    /// 首帧延迟（毫秒，自 probe 开始到帧总线出现第一帧）；0 = 未出帧。
    pub first_frame_ms: u64,
}

/// 独立直播间解码冒烟测试（**不占用全局 LIVE 会话，多线程并行安全**）。
///
/// 用途：分区批量测试（tests/live_batch.rs）——完整复用生产解码链
/// （resolve_stream_sync → decode_session / decode_hls_session），运行
/// `secs` 秒后经 stop 信号停止并汇总统计。`allow_degrade=false`：测试中
/// 不降清晰度重连，单次尝试的成功/失败即结论（生产由 decode_loop 负责降级）。
pub fn probe_room(room: &str, secs: u64) -> ProbeRoomResult {
    let t0 = std::time::Instant::now();
    let mut res = ProbeRoomResult {
        is_hls: false,
        ok: false,
        error: None,
        frames: 0,
        fps: 0.0,
        w: 0,
        h: 0,
        elapsed_ms: 0,
        first_frame_ms: 0,
    };

    // 1) 解析流地址（FLV 优先、HLS 回退；与生产同一函数）。
    let candidates = match resolve_stream_sync(room, 10000) {
        Ok((c, _)) if !c.is_empty() => c,
        Ok(_) => {
            res.error = Some("无可用流地址".to_string());
            res.elapsed_ms = t0.elapsed().as_millis() as u64;
            return res;
        }
        Err(e) => {
            res.error = Some(e);
            res.elapsed_ms = t0.elapsed().as_millis() as u64;
            return res;
        }
    };
    let (url, referer) = candidates[0].clone();
    res.is_hls = url.contains(".m3u8");

    // 2) 独立会话：自建 stop 与帧总线（与 decode_loop 相同的组装方式）。
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(Mutex::new(Frames {
        queue: std::collections::VecDeque::new(),
        latest: None,
        scan_cache: None,
        next_seq: 0,
    }));
    let (s2, f2) = (stop.clone(), frames.clone());
    let paused = Arc::new(AtomicBool::new(false)); // 探针不暂停
    let is_hls = res.is_hls;
    let handle = std::thread::Builder::new()
        .name("probe-decode".into())
        .spawn(move || {
            if is_hls {
                decode_hls_session(&url, &referer, &s2, &f2, &paused)
            } else {
                decode_session(&url, &referer, None, &s2, &f2, false, &paused)
            }
        });
    let handle = match handle {
        Ok(h) => h,
        Err(e) => {
            res.error = Some(format!("解码线程启动失败: {e}"));
            res.elapsed_ms = t0.elapsed().as_millis() as u64;
            return res;
        }
    };

    // 3) 运行指定时长后停止（解码循环在读超时边界检查 stop，最坏多等一个
    //    读超时：FLV 3 秒 / HLS 单请求级）。期间轮询帧总线记录首帧时刻。
    let first_frame_ms = wait_with_first_frame(&frames, secs);
    stop.store(true, Ordering::SeqCst);
    let decode_result = handle
        .join()
        .unwrap_or_else(|_| Err("解码线程异常退出".to_string()));

    // 4) 统计：帧总线 next_seq 即出帧数；latest 提供分辨率。
    let (frames_n, w, h) = {
        let f = frames.lock().unwrap_or_else(PoisonError::into_inner);
        (
            f.next_seq as u32,
            f.latest.as_ref().map_or(0, |x| x.w),
            f.latest.as_ref().map_or(0, |x| x.h),
        )
    };
    res.frames = frames_n;
    res.w = w;
    res.h = h;
    res.fps = frames_n as f64 / secs.max(1) as f64;
    res.elapsed_ms = t0.elapsed().as_millis() as u64;
    res.first_frame_ms = first_frame_ms;
    match decode_result {
        Ok(()) => {
            res.ok = frames_n > 0;
            if !res.ok {
                res.error = Some("测试时长内无视频帧".to_string());
            }
        }
        Err(e) => {
            // 中途错误：有帧则属「部分失败」（分析时按 frames 区分），保留 error。
            res.ok = false;
            res.error = Some(e);
        }
    }
    res
}

/// 等待 secs 秒（每 5ms 轮询），返回帧总线出现第一帧的时刻（毫秒）；未出帧返回 0。
/// 用轮询而非固定 sleep，以便精确测首帧延迟（端到端延迟的核心指标）。
pub(super) fn wait_with_first_frame(frames: &Arc<Mutex<Frames>>, secs: u64) -> u64 {
    let start = std::time::Instant::now();
    let deadline = Duration::from_secs(secs);
    let mut first_ms = 0u64;
    loop {
        let elapsed = start.elapsed();
        if elapsed >= deadline {
            return first_ms;
        }
        if first_ms == 0
            && frames
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .next_seq
                > 0
        {
            first_ms = elapsed.as_millis() as u64;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 按平台 + 房间号做完整生产链冒烟测试（解析 → 解码）。
/// 抖音房间用 web_rid，走 `resolve_source_sync`（ttwid + enter）。
pub fn probe_room_platform(platform: Platform, room: &str, secs: u64) -> ProbeRoomResult {
    let t0 = std::time::Instant::now();
    let mut res = ProbeRoomResult {
        is_hls: false,
        ok: false,
        error: None,
        frames: 0,
        fps: 0.0,
        w: 0,
        h: 0,
        elapsed_ms: 0,
        first_frame_ms: 0,
    };
    let src = match resolve_source_sync(platform, room, 10000) {
        Ok(s) if !s.candidates.is_empty() => s,
        Ok(_) => {
            res.error = Some("无可用流地址".to_string());
            res.elapsed_ms = t0.elapsed().as_millis() as u64;
            return res;
        }
        Err(e) => {
            res.error = Some(e);
            res.elapsed_ms = t0.elapsed().as_millis() as u64;
            return res;
        }
    };
    let (url, referer) = src.candidates[0].clone();
    let mut r2 = probe_flv_url(&url, &referer, src.cookie.as_deref(), secs);
    // 保留解析耗时计入总耗时。
    r2.elapsed_ms += t0.elapsed().as_millis() as u64;
    r2
}

/// 按给定 FLV/HLS 直链做解码冒烟测试（诊断用）。`cookie` 可选（抖音直链无需）。
/// 与 `probe_room` 同构，但跳过解析步骤——用于验证「已有直链」的解码链健康度。
pub fn probe_flv_url(url: &str, referer: &str, cookie: Option<&str>, secs: u64) -> ProbeRoomResult {
    let t0 = std::time::Instant::now();
    let mut res = ProbeRoomResult {
        is_hls: url.contains(".m3u8"),
        ok: false,
        error: None,
        frames: 0,
        fps: 0.0,
        w: 0,
        h: 0,
        elapsed_ms: 0,
        first_frame_ms: 0,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(Mutex::new(Frames {
        queue: std::collections::VecDeque::new(),
        latest: None,
        scan_cache: None,
        next_seq: 0,
    }));
    let (s2, f2) = (stop.clone(), frames.clone());
    let is_hls = res.is_hls;
    let (url, referer, cookie) = (
        url.to_string(),
        referer.to_string(),
        cookie.map(std::string::ToString::to_string),
    );
    let paused = Arc::new(AtomicBool::new(false)); // 探针不暂停
    let handle = std::thread::Builder::new()
        .name("probe-flv".into())
        .spawn(move || {
            if is_hls {
                decode_hls_session(&url, &referer, &s2, &f2, &paused)
            } else {
                decode_session(&url, &referer, cookie.as_deref(), &s2, &f2, false, &paused)
            }
        });
    let handle = match handle {
        Ok(h) => h,
        Err(e) => {
            res.error = Some(format!("解码线程启动失败: {e}"));
            res.elapsed_ms = t0.elapsed().as_millis() as u64;
            return res;
        }
    };
    let first_frame_ms = wait_with_first_frame(&frames, secs);
    stop.store(true, Ordering::SeqCst);
    let decode_result = handle
        .join()
        .unwrap_or_else(|_| Err("解码线程异常退出".to_string()));
    let (frames_n, w, h) = {
        let f = frames.lock().unwrap_or_else(PoisonError::into_inner);
        (
            f.next_seq as u32,
            f.latest.as_ref().map_or(0, |x| x.w),
            f.latest.as_ref().map_or(0, |x| x.h),
        )
    };
    res.frames = frames_n;
    res.w = w;
    res.h = h;
    res.fps = frames_n as f64 / secs.max(1) as f64;
    res.elapsed_ms = t0.elapsed().as_millis() as u64;
    res.first_frame_ms = first_frame_ms;
    match decode_result {
        Ok(()) => {
            res.ok = frames_n > 0;
            if !res.ok {
                res.error = Some("测试时长内无视频帧".to_string());
            }
        }
        Err(e) => {
            res.ok = false;
            res.error = Some(e);
        }
    }
    res
}
