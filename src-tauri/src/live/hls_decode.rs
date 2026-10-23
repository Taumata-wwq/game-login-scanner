use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::bili::*;
use crate::decode::new_decoder;

use super::decode::{decode_and_push, DecodeOutcome, DECODE_FAIL_LIMIT};
use super::frames::*;
// ---------- HLS 路径：分片下载 → AVCC 转 Annex-B → 解码 ----------

/// HLS 会话：拉 m3u8 -> init 段取 SPS/PPS -> 循环拉媒体段解码。
///
/// 低延迟策略：LL-HLS 1 秒分片，每轮取 seq 比上次新的分片；首次或积压时跳到
/// 最近的关键帧段（丢弃中间积压），把延迟压在 1~2 个分片内。
pub(super) fn decode_hls_session(
    m3u8_url: &str,
    referer: &str,
    stop: &AtomicBool,
    frames: &Arc<Mutex<Frames>>,
    paused: &AtomicBool,
) -> Result<(), String> {
    let headers = vec![("User-Agent".to_string(), BROWSER_UA.to_string())];
    // P0：HLS 拉流全程复用同一 TCP+TLS 连接（实测 m3u8 150→30ms，且避免 CDN
    // 对快速重连的拒绝）。host 变化时 KeepAliveConn 自动重连。
    let mut conn = crate::http::KeepAliveConn::new();

    // 1) 拉 m3u8 播放列表。
    let m3u8_bytes = conn.get(m3u8_url, &headers, referer)?;
    let m3u8_text = String::from_utf8_lossy(&m3u8_bytes);
    let playlist =
        crate::hls::parse_m3u8(&m3u8_text, m3u8_url).ok_or_else(|| "m3u8 解析失败".to_string())?;
    let init_url = playlist
        .init_url
        .clone()
        .ok_or_else(|| "m3u8 缺少 init 段".to_string())?;

    // 2) 拉 init 段，取 SPS/PPS 与 timescale。
    let init_bytes = conn.get(&init_url, &headers, referer)?;
    let init_info = crate::hls::parse_init(&init_bytes)
        .ok_or_else(|| "init 段解析失败（无 avcC）".to_string())?;
    let timescale = init_info.timescale.max(1) as u64;
    let track_id = init_info.track_id;
    log::info!(
        "HLS: init 段 OK（timescale={timescale}，参数集 {} 字节）",
        init_info.params.len()
    );

    // 3) 解码器：Media Foundation H.264 硬解（系统组件，见 decode.rs）。
    let mut decoder = new_decoder().map_err(|e| format!("解码器初始化失败: {e}"))?;
    log::info!("HLS 解码器：{}", decoder.name());
    // 喂 SPS/PPS（Media Foundation 直接接受标准 Annex-B 参数集）。
    if let Err(e) = decoder.decode(&init_info.params, 0) {
        log::warn!("HLS 参数集解码失败: {e}");
    }

    let mut last_seq = playlist.media_sequence.saturating_sub(1);
    let mut skipped_first = false;
    let mut fail_streak = 0u32;
    // 连续解码失败计数（阈值与 FLV 路径相同，见 DECODE_FAIL_LIMIT），到限即报错走
    // 降级/重连。与 FLV 的差别：这里把「解码调用成功但零输出」也算作失败——HLS 没有
    // FLV 那样的逐 tag 心跳，「分片能下、画面解不出来」的流若不计数就会永远占带宽与
    // CPU 却零帧零报错。两个分项只为让日志能区分成因。
    let mut dec_fails = 0u32;
    let mut dec_none = 0u32;
    let mut dec_err = 0u32;
    // P1/P3：按分片边界调度轮询——无新分片时轮询间隔取 target_duration/4
    // （1 秒分片 → 250ms），夹在 [150, 400]ms；不再用固定 300ms。
    let poll_ms = (playlist.target_duration * 1000 / 4).clamp(150, 400);
    // 延迟统计：用「积压分片数」作为代理——服务端最新分片 seq − 已消费 seq。
    // （HLS 是整片下载后批量解码，样本级墙钟锚点法不适用，会得负值。）
    // 积压 × target_duration ≈ 端到端延迟。每 5 秒输出一次。
    let mut backlog_sum: u64 = 0;
    let mut backlog_n: u32 = 0;
    let mut lag_window = std::time::Instant::now();

    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        // 后台暂停：不拉新分片（零带宽零解码），恢复时跳到最新关键帧段。
        if paused.load(Ordering::SeqCst) {
            skipped_first = false;
            lag_window = std::time::Instant::now();
            std::thread::sleep(Duration::from_millis(120));
            continue;
        }
        // 拉最新 m3u8。
        let text = match conn.get(m3u8_url, &headers, referer) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                log::warn!("HLS: 拉取 m3u8 失败: {e}");
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let Some(pl) = crate::hls::parse_m3u8(&text, m3u8_url) else {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        // 延迟统计：积压分片数 = 服务端最新 seq − 已消费 seq。
        if let Some(latest) = pl.segments.last() {
            backlog_sum += latest.seq.saturating_sub(last_seq);
            backlog_n += 1;
        }
        // 收集新分片。
        let fresh: Vec<&crate::hls::Segment> =
            pl.segments.iter().filter(|s| s.seq > last_seq).collect();
        if fresh.is_empty() {
            // P3：按分片边界节奏轮询，尽快发现新分片。
            std::thread::sleep(Duration::from_millis(poll_ms));
            continue;
        }
        // 首次或积压：从最近的关键帧段开始（保证解码器有随机访问点）。
        let start = if !skipped_first || fresh.len() > 3 {
            skipped_first = true;
            fresh.iter().rposition(|s| s.keyframe).unwrap_or(0)
        } else {
            0
        };
        if start > 0 {
            log::info!("HLS: 追帧，跳过 {} 个积压分片", start);
        }
        for seg in &fresh[start..] {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let seg_bytes = match conn.get(&seg.url, &headers, referer) {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("HLS: 分片 {} 下载失败: {e}", seg.seq);
                    fail_streak += 1;
                    if fail_streak >= 5 {
                        return Err(format!("HLS 分片连续下载失败: {e}"));
                    }
                    last_seq = seg.seq;
                    continue;
                }
            };
            fail_streak = 0;
            last_seq = seg.seq;
            let samples = crate::hls::parse_segment(&seg_bytes, track_id);
            for s in samples {
                let Some(end) = s
                    .offset
                    .checked_add(s.size)
                    .filter(|end| *end <= seg_bytes.len())
                else {
                    continue;
                };
                let annexb = avcc_to_annexb(&seg_bytes[s.offset..end]);
                if annexb.is_empty() {
                    continue;
                }
                let media_ts = (s.time * 1000 / timescale) as u32;
                match decode_and_push(&mut decoder, &annexb, media_ts, frames) {
                    DecodeOutcome::Frames => dec_fails = 0,
                    kind => {
                        dec_fails += 1;
                        match kind {
                            DecodeOutcome::None => dec_none += 1,
                            _ => dec_err += 1,
                        }
                        if dec_fails.is_multiple_of(20) {
                            log::warn!(
                                "HLS: H.264 连续无输出/解码失败 {dec_fails} 次（无输出 {dec_none} / 错误 {dec_err}）"
                            );
                        }
                        if dec_fails >= DECODE_FAIL_LIMIT {
                            return Err(format!(
                                "H.264 解码持续失败（连续 {dec_fails} 次：无输出 {dec_none} / 错误 {dec_err}），降级清晰度重试"
                            ));
                        }
                    }
                }
            }
        }
        if lag_window.elapsed() >= Duration::from_secs(5) && backlog_n > 0 {
            let avg_backlog = backlog_sum as f64 / backlog_n as f64;
            log::info!(
                "HLS 延迟: 平均积压 {:.1} 分片 ≈ {:.0}ms",
                avg_backlog,
                avg_backlog * playlist.target_duration as f64 * 1000.0
            );
            backlog_sum = 0;
            backlog_n = 0;
            lag_window = std::time::Instant::now();
        }
        // P2：消费后立即重新轮询（仅 15ms 让步避免空转）——下载+解码期间
        // 可能已有新分片就绪，立即捕获可省去一个完整轮询间隔的等待。
        std::thread::sleep(Duration::from_millis(15));
    }
}
