use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::decode::{new_decoder, DecodedFrame, VideoDecoder};

use super::bili::*;
use super::flv_reader::{read_exact, HttpFlvReader};
use super::frames::stats;
use super::frames::*;
use super::hls_decode::decode_hls_session;
use super::session::*;
///
/// 2026-10-11（MF 硬解落地后）：解码吞吐大幅改善（实测 720p 平均单帧
/// 14.6ms、1080p 同量级，远低于典型帧间隔），追帧从「常态工作」退化为
/// 安全网。阈值由 400ms 下调到 300ms：吞吐充分时追帧仍不触发，而偶发
/// 卡顿/慢帧场景能更快把显示延迟收回来。
/// 阈值越低延迟越低、跳帧越频繁；300-600ms 是「扫码抢码」场景的合理折中。
pub(super) const CATCHUP_LAG_MS: i64 = 300;

/// 解码持续失败的容忍次数：超过则降级清晰度重试。
pub(super) const DECODE_FAIL_LIMIT: u32 = 40;

/// 持续追帧上限：连续快速追帧（间隔 < 4s）达到此次数，判为解码吞吐持续不足，
/// 降级清晰度换低码率流。仅「持续跟不上」才降级，偶发追帧不触发。
pub(super) const CATCHUP_LIMIT: u32 = 4;

/// 「连续追帧」判定的时间窗口：两次追帧间隔超过此值即视为偶发，不累计。
pub(super) const CATCHUP_WINDOW_MS: u64 = 4000;

/// 解码线程向 start() 回报的致命失败原因（未开播 / 解码不支持 / 连续重连失败）。
/// start() 轮询它以便快速返回明确错误，而非干等 15 秒首帧超时。
pub(super) type FailFlag = Arc<Mutex<Option<String>>>;

/// 记录致命失败并让解码线程返回。
pub(super) fn set_fail(fail: &FailFlag, reason: &str) {
    *fail.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason.to_string());
}

/// 该错误是否属「本平台/本构建不具备解码能力」——重连毫无意义的那一类。
///
/// 判定依据是**能力性**措辞，而非任何临时故障（网络、节点劣化、流结束、追帧）
/// 都仍走原有重连路径。当前唯一来源是 Android 的 MediaCodec 骨架
/// （`android/mediacodec_decode.rs` 返回「尚未实现」，阶段 4 落地后此分支自然失效）。
pub(super) fn is_fatal_decode_error(e: &str) -> bool {
    e.contains("尚未实现") || e.contains("不支持该平台") || e.contains("解码器不可用")
}

pub(super) fn decode_loop(
    platform: Platform,
    room: String,
    stop: Arc<AtomicBool>,
    frames: Arc<Mutex<Frames>>,
    fail: FailFlag,
    paused: Arc<AtomicBool>,
) {
    // 清晰度降级链：B站从首次解析响应顺带得到（避免二次 API 请求），
    // 抖音无档位概念。首次 resolve 前暂用 [10000] 兜底。
    let mut qn_chain: Vec<u32> = vec![10000];
    let mut qn_chain_ready = false;
    let mut fails = 0u32;
    let mut qn_idx = 0usize;
    // 候选节点池（多 CDN 节点轮换：固定一个可能撞上慢/故障节点）。
    let mut candidates: Vec<(String, String)> = Vec::new();
    // 抖音拉流需带 ttwid Cookie（B站为 None）。
    let mut cookie: Option<String> = None;
    let mut cand_used = 0usize;
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if candidates.is_empty() {
            match resolve_source_sync(platform, &room, qn_chain[qn_idx]) {
                Ok(src) => {
                    candidates = src.candidates;
                    cookie = src.cookie;
                    // 首次解析顺带拿到档位链（B站）；抖音保持 [10000]。
                    if !qn_chain_ready {
                        if let Some(chain) = src.qn_chain {
                            if !chain.is_empty() {
                                log::info!(
                                    "{} 房间 {room} 清晰度档位链: {chain:?}",
                                    platform.label()
                                );
                                qn_chain = chain;
                            }
                        }
                        qn_chain_ready = true;
                    }
                }
                Err(e) => {
                    log::warn!("解析流地址失败: {e}");
                    // 下播/未开播：API 已明确判定，重试无意义——立即关闭会话，
                    // 让前端迅速切回屏幕预览（否则要等节点重连 + 多次解析重试，
                    // 画面停格数秒）。
                    if e.contains("未开播") {
                        log::info!("直播间已下播，立即关闭会话");
                        set_fail(&fail, &format!("{}直播间 {room} 已下播", platform.label()));
                        remove_session_if(&frames);
                        return;
                    }
                    fails += 1;
                    if fails > 4 {
                        log::error!("直播流连续解析失败，会话关闭");
                        set_fail(
                            &fail,
                            &format!("{}直播间 {room} 流地址解析失败：{e}", platform.label()),
                        );
                        remove_session_if(&frames);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(400));
                    continue;
                }
            }
            cand_used = 0;
        }
        if cand_used >= candidates.len() {
            // 本轮候选全部失败：重新解析（新 expires 自愈 + 轮换节点）。
            candidates.clear();
            continue;
        }
        let (url, referer) = candidates[cand_used].clone();
        cand_used += 1;
        let is_hls = url.contains(".m3u8");
        log::info!(
            "连接直播流节点 {cand_used}/{}（{}）…",
            candidates.len(),
            if is_hls { "HLS" } else { "FLV" }
        );
        let allow_degrade = qn_idx + 1 < qn_chain.len();
        let session_result = if is_hls {
            decode_hls_session(&url, &referer, &stop, &frames, &paused)
        } else {
            decode_session(
                &url,
                &referer,
                cookie.as_deref(),
                &stop,
                &frames,
                allow_degrade,
                &paused,
            )
        };
        match session_result {
            Ok(()) => return, // 收到停止信号
            // 解码器持续失败（编码不兼容/解码跟不上）：降级清晰度换低码率流重试。
            Err(e) if e.starts_with("H.264 解码持续失败") => {
                if qn_idx + 1 < qn_chain.len() {
                    qn_idx += 1;
                    fails = 0;
                    candidates.clear();
                    log::info!("清晰度降级到 qn={} 重试", qn_chain[qn_idx]);
                    continue;
                }
                log::error!("最低清晰度仍无法解码，会话关闭");
                set_fail(
                    &fail,
                    &format!(
                        "{}直播间 {room} 编码格式不受支持（H.264 解码失败）",
                        platform.label()
                    ),
                );
                remove_session_if(&frames);
                return;
            }
            // 解码吞吐持续不足（追帧过频）：降级清晰度换低码率流。
            // （decode_session 只在还能降级时返回此错误，故此处 qn_idx 必可前进。）
            Err(e) if e.starts_with("直播追帧过于频繁") => {
                qn_idx += 1;
                fails = 0;
                candidates.clear();
                log::info!("解码吞吐不足，清晰度降级到 qn={} 重试", qn_chain[qn_idx]);
                continue;
            }
            // 平台解码能力缺失（如 Android 阶段 4 未落地）：**不可恢复**。
            // 每次重连都会在同一个点失败，重试只会制造「连上→立即失败→重连」
            // 风暴（真机实测：每会话 8 次重连 × 多个会话，反复解析拉流、刷爆
            // 日志、白耗流量与电量）。直接上报明确原因并结束会话。
            Err(e) if is_fatal_decode_error(&e) => {
                log::error!("解码能力不可用，终止会话（不重连）: {e}");
                set_fail(&fail, &format!("{}直播间 {room}：{e}", platform.label()));
                remove_session_if(&frames);
                return;
            }
            Err(e) => {
                log::warn!("直播流断开: {e}");
                // 「流已结束」通常是主播下播（而非节点故障）：丢弃剩余候选节点，
                // 直接重新解析以快速判定下播状态——否则会先逐个尝试失效节点，
                // 每次连接超时数秒，画面停格。
                if e.contains("流已结束") {
                    candidates.clear();
                }
            }
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        fails += 1;
        // 下播时 CDN 会快速关闭连接，重连也立即失败——快速放弃并移除会话。
        if fails > 8 {
            log::error!("直播流连续重连失败，会话关闭");
            set_fail(
                &fail,
                &format!(
                    "{}直播间 {room} 连接失败（CDN 节点均不可用）",
                    platform.label()
                ),
            );
            remove_session_if(&frames);
            return;
        }
        log::info!("直播流重连（第 {fails} 次，尝试下一节点）…");
        std::thread::sleep(Duration::from_millis(400));
    }
}

/// 仅当会话池中仍是本线程对应的会话时移除（防止误删同房间重建的新会话）。
/// 用 frames 指针匹配：解码线程只持有自己那帧总线的引用。
pub(super) fn remove_session_if(frames: &Arc<Mutex<Frames>>) {
    let matched_key = {
        let mut sessions = lock_sessions();
        let Some(map) = sessions.as_mut() else {
            return;
        };
        let k = map
            .iter()
            .find(|(_, s)| Arc::ptr_eq(&s.frames, frames))
            .map(|(k, _)| k.clone());
        if let Some(k) = &k {
            map.remove(k);
        }
        k
    };
    if let Some(k) = matched_key {
        log::info!("直播会话已关闭（流结束/重连失败），回退屏幕预览与扫码");
        // 先释放会话锁再清保活：set_scan_keepalive 会 refresh_paused，
        // 其加锁顺序是 keepalive -> sessions，持锁调用会反向加锁。
        set_scan_keepalive(&k, false);
    }
}

/// HLS 单次解码调用的结果：调用方需要区分「解码器报错」与「调用成功但零输出」，
/// 两者都计入连续失败（见调用点的说明），但日志与错误文案要能区分成因。
pub(super) enum DecodeOutcome {
    /// 产出至少一帧。
    Frames,
    /// 调用成功但没有任何输出（如只含参数集/SEI 的访问单元）。
    None,
    /// 解码器返回错误。
    Err,
}

/// 解码一个 Annex-B 访问单元并把产出的帧推入帧总线（HLS 路径用）。
/// 单帧错误本身仍静默，但调用方需要连续失败计数——否则「分片能下、H.264 解不出来」
/// 的流会永远占带宽和 CPU 却零帧、零报错。
pub(super) fn decode_and_push(
    decoder: &mut dyn VideoDecoder,
    annexb: &[u8],
    media_ts: u32,
    frames: &Arc<Mutex<Frames>>,
) -> DecodeOutcome {
    match decoder.decode(annexb, media_ts) {
        Ok(list) => {
            let produced = !list.is_empty();
            for of in list {
                push_decoded(frames, media_ts, of);
            }
            if produced {
                DecodeOutcome::Frames
            } else {
                DecodeOutcome::None
            }
        }
        Err(e) => {
            log::debug!("HLS 单帧解码失败: {e}");
            DecodeOutcome::Err
        }
    }
}

/// 把解码产出帧推入帧总线（HLS 与 FLV 路径共用）。
pub(super) fn push_decoded(frames: &Arc<Mutex<Frames>>, media_ts: u32, of: DecodedFrame) {
    let mut f = frames.lock().unwrap_or_else(PoisonError::into_inner);
    f.next_seq += 1;
    let frame = Arc::new(Frame {
        seq: f.next_seq,
        ts: media_ts,
        w: of.w,
        h: of.h,
        y: of.y,
        u: of.u,
        v: of.v,
    });
    f.latest = Some(frame.clone());
    f.queue.push_back(frame);
    while f.queue.len() > FRAME_BUFFER_MAX {
        f.queue.pop_front();
    }
}

pub(super) fn decode_session(
    url: &str,
    referer: &str,
    cookie: Option<&str>,
    stop: &AtomicBool,
    frames: &Arc<Mutex<Frames>>,
    allow_degrade: bool,
    paused: &AtomicBool,
) -> Result<(), String> {
    let mut reader = HttpFlvReader::connect(url, referer, cookie)?;

    // FLV 头（9 字节）+ PreviousTagSize0（4 字节）。
    let mut hdr = Vec::new();
    read_exact(&mut reader, &mut hdr, 13)?;
    if &hdr[0..3] != b"FLV" {
        return Err("响应不是 FLV 流".to_string());
    }

    // 解码器：Media Foundation H.264 硬解（系统组件，见 decode.rs）。
    let mut decoder = new_decoder().map_err(|e| format!("解码器初始化失败: {e}"))?;
    log::info!("FLV 解码器：{}", decoder.name());
    let mut dec_fails = 0u32;
    let (mut out_ok, mut out_none, mut out_err) = (0u32, 0u32, 0u32);

    let mut tag = Vec::new();
    // 解码耗时统计（吞吐诊断：总耗时 / 出帧数）。
    let mut decode_ns_total: u128 = 0;
    // 慢流检测：某节点推流过慢（CDN 故障/限流）时返回错误，上层换节点。
    let mut last_data_at = std::time::Instant::now();
    // 统计窗口：6 秒内视频帧不足 30（≈5fps）判节点劣化——视频 tag 稀疏
    // 而音频不断时 3 秒检测会被绕过，窗口法稳定覆盖。
    let mut window_frames = 0u32;
    let mut window_start = std::time::Instant::now();
    // ---- 直播追帧状态 ----
    // 锚点：(首个视频 tag 的媒体时间戳, 其到达墙钟)。直播媒体时间戳≈推流墙钟，
    // 据此推算「此刻应有的媒体时间戳」，与实际 tag 时间戳之差即落后量。
    let mut anchor: Option<(u32, std::time::Instant)> = None;
    // 是否处于追帧丢弃态（排空积压，等下一个关键帧恢复）。
    let mut skipping = false;
    // 追帧开始时刻（用于超时保护：异常流长时间无关键帧时强制重连，避免假死）。
    let mut skip_since: Option<std::time::Instant> = None;
    // 恢复同步：暂停前记录最后视频 media_ts，恢复时推算「直播边缘」媒体时间戳
    // （≈ 暂停前 ts + 暂停时长），丢弃所有早于边缘的帧，直到边缘处的关键帧开始解码
    // —— 直接跳到最新画面，且不受 socket 积压量影响（比墙钟窗口更精确）。
    let mut last_video_ts: Option<u32> = None;
    let mut pause_at: Option<(u32, std::time::Instant)> = None;
    // 恢复目标媒体时间戳 + 设定时刻（墙钟安全网：超过 2s 仍未追上则强制恢复，
    // 避免时间戳异常导致永久丢弃 → 假死）。
    let mut resume_target: Option<(u32, std::time::Instant)> = None;
    // 连续快速追帧计数（间隔 < CATCHUP_WINDOW_MS 才累加；偶发追帧不复位也不累加）。
    // 达 CATCHUP_LIMIT 判为解码吞吐持续不足 → 降级清晰度（新会话计数从 0 起）。
    let mut catchup_count = 0u32;
    let mut last_catchup_at: Option<std::time::Instant> = None;
    // 缓存的 SPS/PPS（追帧重建解码器后需重喂）。
    let mut param_sets: Vec<u8> = Vec::new();
    // 心跳窗口的增量快照：把「后端解出多少帧」与「前端取走多少帧」放在同一行，
    // 直播帧率上不去时一眼能分出是解码侧还是消费侧（2026-10-22 加）。
    let mut prev_stats = frame_stats();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        // 后台暂停：不读 socket（TCP 背压让 CDN 停发 → 零带宽零解码）。
        // 恢复时进入追帧态（丢弃到最新关键帧），直接跳到最新画面，避免旧帧快进。
        if paused.load(Ordering::SeqCst) {
            anchor = None;
            skipping = false;
            skip_since = None;
            if pause_at.is_none() {
                pause_at = Some((last_video_ts.unwrap_or(0), std::time::Instant::now()));
            }
            last_data_at = std::time::Instant::now();
            window_start = std::time::Instant::now();
            window_frames = 0;
            std::thread::sleep(Duration::from_millis(80));
            continue;
        }
        // 从暂停恢复：推算直播边缘媒体时间戳，后续丢弃早于边缘的帧。
        if let Some((ts0, at)) = pause_at.take() {
            let elapsed = at.elapsed().as_millis() as u32;
            resume_target = Some((
                ts0.wrapping_add(elapsed).wrapping_add(200),
                std::time::Instant::now(),
            ));
        }
        // 连接后 3 秒无任何视频数据：节点劣化，切换节点重连。
        if last_data_at.elapsed() > Duration::from_secs(3) {
            return Err("流传输过慢（CDN 节点劣化），切换节点重连".to_string());
        }
        // 6 秒窗口帧数不足：节点只推音频+零星视频的劣化形态。
        if window_start.elapsed() >= Duration::from_secs(6) {
            if window_frames < 30 {
                return Err("流传输过慢（6 秒不足 30 帧，CDN 节点劣化），切换节点重连".to_string());
            }
            window_start = std::time::Instant::now();
            window_frames = 0;
        }
        // Tag 头 11 字节：type(1) data_size(3) timestamp(3+1) stream_id(3)。
        read_exact(&mut reader, &mut tag, 11)?;
        let tag_type = tag[0];
        let data_size = ((tag[1] as usize) << 16) | ((tag[2] as usize) << 8) | tag[3] as usize;
        // FLV 时间戳（媒体时钟，ms）：低 24 位在 [4..7]，高 8 位在 [7]。
        // 必须在读 body 前提取（read_exact 会覆盖 tag 缓冲）。
        let media_ts = ((tag[7] as u32) << 24)
            | ((tag[4] as u32) << 16)
            | ((tag[5] as u32) << 8)
            | tag[6] as u32;
        if data_size > 8 * 1024 * 1024 {
            return Err("FLV Tag 超过大小上限".to_string());
        }
        read_exact(&mut reader, &mut tag, data_size + 4)?; // 体 + PreviousTagSize
        if tag_type != 9 || data_size < 5 {
            // 音频等非视频 tag 不断不代表流正常——慢流节拍只看视频数据。
            continue;
        }
        last_data_at = std::time::Instant::now();
        window_frames += 1;
        stats::bump(&stats::VIDEO_TAGS, 1);
        let body = &tag[..data_size];
        let first = body[0];
        let frame_type = first >> 4;
        match frame_type {
            1 => stats::bump(&stats::KEY, 1),
            2 => stats::bump(&stats::INTER, 1),
            3 => stats::bump(&stats::DISPOSABLE, 1),
            _ => {}
        }
        if first & 0x0F != 7 {
            continue; // 非 AVC
        }
        last_video_ts = Some(media_ts);

        // ---- 恢复同步：丢弃早于「直播边缘」的积压帧，在边缘处关键帧恢复 ----
        // 动机：暂停恢复后 socket/TCP 可能残留积压旧帧，逐帧播出表现为「慢速快进」。
        // 做法：以「暂停前 ts + 暂停时长」为直播边缘，丢弃所有 media_ts < 边缘的帧；
        // 到达边缘后的首个关键帧即开始解码（直接跳到最新画面）。
        // 安全网：超过 2s 仍未追上（时间戳异常/积压过深）则在下一个关键帧强制恢复。
        if let Some((target, set_at)) = resume_target {
            let before = (media_ts.wrapping_sub(target) as i32) < 0;
            let timed_out = set_at.elapsed() > Duration::from_secs(2);
            if (before && !timed_out) || frame_type != 1 {
                continue; // 尚未追上直播边缘（且未超时），或未到关键帧
            }
            // 追上边缘（或超时）+ 关键帧：恢复正常解码。
            resume_target = None;
            anchor = Some((media_ts, std::time::Instant::now()));
        }

        // ---- 直播追帧（安全网）----
        // 落后量 = 应有媒体时间戳（锚点 + 已过墙钟）− 当前 tag 媒体时间戳。
        // 解码吞吐不足时 socket 缓冲持续积压、落后量单调增长 → 触发追帧；
        // 追帧后锚点重置。asm 生效后正常流落后量恒 0（实测 35 秒零触发）。
        let now = std::time::Instant::now();
        // 落后量（>0 = 解码落后于推流）。下面「跳 disposable 帧」也用它做判据。
        let mut lag_ms: i64 = 0;
        match anchor {
            None => anchor = Some((media_ts, now)),
            Some((a_ts, a_at)) => {
                let expected = a_ts as i64 + a_at.elapsed().as_millis() as i64;
                let lag = expected - media_ts as i64;
                lag_ms = lag;
                if !skipping && lag > CATCHUP_LAG_MS {
                    skipping = true;
                    skip_since = Some(now);
                    // 仅「连续快速」追帧才累加：与上次追帧间隔 < CATCHUP_WINDOW_MS
                    // 视为持续跟不上；间隔过长的偶发追帧不累加（否则长健康会话会误降级）。
                    let rapid = last_catchup_at.is_some_and(|t| {
                        now.duration_since(t) < Duration::from_millis(CATCHUP_WINDOW_MS)
                    });
                    if rapid {
                        catchup_count += 1;
                    } else {
                        catchup_count = 1;
                    }
                    last_catchup_at = Some(now);
                    log::info!(
                        "直播追帧：落后 {lag}ms（连续第 {catchup_count} 次），丢弃至下一关键帧"
                    );
                }
            }
        }
        if skipping {
            // 超时保护：追帧态持续过久仍无关键帧（流异常/丢 SPS）→ 重连，避免假死。
            if skip_since.is_some_and(|t| t.elapsed() > Duration::from_secs(8)) {
                return Err("直播追帧超时（长时间无关键帧），重连".to_string());
            }
            // 只读不解码，快速排空 socket 缓冲；遇关键帧（IDR）重建解码器恢复。
            if frame_type == 1 && body[1] == 1 && !param_sets.is_empty() {
                decoder = new_decoder().map_err(|e| format!("解码器重建失败: {e}"))?;
                let _ = decoder.decode(&param_sets, media_ts);
                skipping = false;
                skip_since = None;
                anchor = Some((media_ts, std::time::Instant::now()));
                log::info!("直播追帧：已在关键帧恢复（ts={media_ts}ms）");
                // 本会话追帧达上限：解码吞吐跟不上码率。仅在还能降级时返回错误；
                // 已是最低清晰度则继续运行（追帧本身已能维持延迟，重连反而中断画面）。
                if catchup_count >= CATCHUP_LIMIT && allow_degrade {
                    return Err("直播追帧过于频繁（解码吞吐不足），降级清晰度重试".to_string());
                }
            } else {
                continue;
            }
        }

        // 非参考帧（frame_type=3，disposable inter）不影响后续解码的参考链，跳过
        // 可近乎减半解码负载；但它同样是一帧画面——正常状态下必须解码，否则带时间
        // 分层的流（30fps 金字塔编码）会被砍成 15fps。只在本轮已经落后（解码跟不上）
        // 时跳过，作为「不解清晰度也先追上」的软阀门。
        if frame_type == 3 && body[1] == 1 && lag_ms > DISPOSABLE_SKIP_LAG_MS {
            stats::bump(&stats::DISPOSABLE_SKIPPED, 1);
            continue;
        }

        match body[1] {
            0 => {
                // AVC sequence header：AVCDecoderConfigurationRecord → SPS/PPS。
                param_sets = avcc_config_to_annexb(&body[5..]);
                log::info!(
                    "AVC 序列参数集: {} 字节（体前 8 字节 {:02x?}）",
                    param_sets.len(),
                    &body[..8.min(body.len())]
                );
                if !param_sets.is_empty() {
                    log::info!(
                        "参数集前 16 字节: {:02x?}",
                        &param_sets[..param_sets.len().min(16)]
                    );
                    if let Err(e) = decoder.decode(&param_sets, media_ts) {
                        log::warn!("参数集解码失败: {e}");
                    }
                }
            }
            1 => {
                // 体布局：flags(1) + AVCPacketType(1) + CompositionTime(3) + AVCC NALU。
                // NoFlush 下参考链持续有效，无需关键帧前重复拼接参数集。
                let annexb = avcc_to_annexb(&body[5..]);
                if annexb.is_empty() {
                    continue;
                }
                // 组合时间（body[2..5]）忽略——显示延迟可接受。
                // 解码错误不静默：持续失败（编码不兼容等）由上层降级清晰度。
                let decode_t = std::time::Instant::now();
                stats::bump(&stats::DECODE_CALLS, 1);
                match decoder.decode(&annexb, media_ts) {
                    Ok(list) => {
                        decode_ns_total += decode_t.elapsed().as_nanos();
                        dec_fails = 0; // 成功即复位：dec_fails 是「连续失败」计数
                        if list.is_empty() {
                            out_none += 1;
                        } else {
                            out_ok += 1;
                            stats::bump(&stats::DECODE_OUT, list.len() as u64);
                            for of in list {
                                push_decoded(frames, media_ts, of);
                            }
                        }
                    }
                    Err(e) => {
                        out_err += 1;
                        dec_fails += 1;
                        if dec_fails == 1 || dec_fails.is_multiple_of(20) {
                            log::warn!("H.264 解码失败（{dec_fails} 次）: {e}");
                        }
                        if dec_fails >= DECODE_FAIL_LIMIT {
                            return Err(
                                "H.264 解码持续失败（编码不兼容或解码器能力不足），降级清晰度重试"
                                    .to_string(),
                            );
                        }
                    }
                }
                // 每 50 个视频 tag 心跳（诊断用）：输入滞后 / 输出滞后 / 队列深度。
                if (out_ok + out_none + out_err) % 50 == 0 {
                    // out_lag 用「显示队列最旧帧」（即将被播出的帧）的媒体时间戳，
                    // 而非 latest——追帧恢复过渡期 latest 尚未更新，用它会把
                    // 「追帧跳变」误报成持续高延迟（实测尖峰虚高至 1.9~3.5s）。
                    // 队列为空时显示端无积压，回退到当前 tag 时间戳（≈输入延迟）。
                    let (qlen, out_ts) = {
                        let f = frames.lock().unwrap_or_else(PoisonError::into_inner);
                        let ts = f.queue.front().map(|b| b.ts).unwrap_or(media_ts);
                        (f.queue.len(), ts)
                    };
                    let in_lag = anchor.as_ref().map_or(0, |(a_ts, a_at)| {
                        *a_ts as i64 + a_at.elapsed().as_millis() as i64 - media_ts as i64
                    });
                    let out_lag = anchor.as_ref().map_or(0, |(a_ts, a_at)| {
                        *a_ts as i64 + a_at.elapsed().as_millis() as i64 - out_ts as i64
                    });
                    let avg_ms = if out_ok > 0 {
                        decode_ns_total as f64 / out_ok as f64 / 1.0e6
                    } else {
                        0.0
                    };
                    let st = frame_stats();
                    let d = |a: u64, b: u64| b.saturating_sub(a);
                    log::info!(
                        "解码心跳: 出帧 {out_ok} / 无输出 {out_none} / 错误 {out_err}，队列 {qlen}，输入滞后 {in_lag}ms，输出滞后 {out_lag}ms，平均单帧解码 {avg_ms:.2}ms │ 本轮 tag {} key {} inter {} disp {} 跳disp {} │ 解码调用 {} 产出 {} │ 预览调用 {} 新帧 {} 空 {} 无会话 {}",
                        d(prev_stats.video_tags, st.video_tags),
                        d(prev_stats.key, st.key),
                        d(prev_stats.inter, st.inter),
                        d(prev_stats.disposable, st.disposable),
                        d(prev_stats.disposable_skipped, st.disposable_skipped),
                        d(prev_stats.decode_calls, st.decode_calls),
                        d(prev_stats.decode_out, st.decode_out),
                        d(prev_stats.preview_calls, st.preview_calls),
                        d(prev_stats.preview_frame, st.preview_frame),
                        d(prev_stats.preview_empty, st.preview_empty),
                        d(prev_stats.preview_nosession, st.preview_nosession),
                    );
                    prev_stats = st;
                }
                // 每 300 个视频 tag（约 5 秒）摘要一次解码结果分布（诊断用）。
                if (out_ok + out_none + out_err) % 300 == 0 {
                    log::info!(
                        "解码摘要: 出帧 {out_ok} / 无输出 {out_none} / 错误 {out_err}（当前清晰度 qn）"
                    );
                }
            }
            _ => {}
        }
    }
}

// ---------- 独立冒烟测试接口（诊断用，2026-10-12 加入） ----------
