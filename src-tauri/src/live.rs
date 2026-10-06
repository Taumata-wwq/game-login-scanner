//! 直播流捕获：B站/抖音 http-flv → FLV 解复用 → openh264 H.264 解码 → RGBA 帧总线。
//!
//! 低延迟链路：平台 API（免登录）解析出 http-flv 拉流地址 → 专用流式连接边下边解
//! （逐 Tag 解复用，解码即存共享帧），延迟主要由平台 CDN 决定（实测约 2~5 秒）。
//! 共享帧供两条消费路径：
//! - 预览：`crop_frame` 按前端视口裁剪降采样（等价屏幕预览协议）；
//! - 扫码：`take_scan_frame` 取新帧走既有二维码识别管线（seq 去重）。

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use openh264::formats::YUVSource;

use crate::http::header_utf8;

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

// ---------- 流地址解析 ----------

/// 解析 B站直播间为 http-flv 拉流地址，返回 (url, referer)。
async fn resolve_stream(room: &str) -> Result<(String, String), String> {
    {
        {
            let headers = vec![
                ("User-Agent".to_string(), header_utf8(BROWSER_UA)),
                (
                    "Referer".to_string(),
                    format!("https://live.bilibili.com/{room}"),
                ),
            ];
            let url = format!(
                "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={room}&protocol=0,1&format=0,2&codec=0,1&qn=10000&platform=web&ptype=16"
            );
            let resp = crate::http::get(&url, headers.clone())
                .await
                .map_err(|e| format!("B站直播接口请求失败: {e}"))?;
            let v: Value = serde_json::from_str(&resp.body).map_err(|_| {
                format!(
                    "B站直播接口响应非 JSON: {}",
                    &resp.body[..resp.body.len().min(120)]
                )
            })?;
            if v.get("code").and_then(|x| x.as_i64()) != Some(0) {
                let msg = v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("接口错误");
                return Err(format!("B站直播间 {room} 获取失败: {msg}"));
            }
            // 首选：v2 结构化流（url_list 非空时）。
            if let Some(u) = parse_bili_flv(&v) {
                return Ok((u, format!("https://live.bilibili.com/{room}")));
            }
            // 回退：旧版 playUrl 接口 durl 直出完整 FLV 地址（免登录场景下
            // v2 的 url_list 常被服务端置空，旧接口反而稳定可用）。
            let real_room = v["data"]["room_id"]
                .as_i64()
                .map(|x| x.to_string())
                .unwrap_or_else(|| room.to_string());
            let url2 = format!(
                "https://api.live.bilibili.com/room/v1/Room/playUrl?cid={real_room}&qn=10000&platform=web"
            );
            let resp2 = crate::http::get(&url2, headers.clone())
                .await
                .map_err(|e| format!("B站直播接口请求失败: {e}"))?;
            let v2: Value = serde_json::from_str(&resp2.body).map_err(|_| {
                format!(
                    "B站直播接口响应非 JSON: {}",
                    &resp2.body[..resp2.body.len().min(120)]
                )
            })?;
            if v2.get("code").and_then(|x| x.as_i64()) != Some(0) {
                let msg = v2
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("接口错误");
                return Err(format!("B站直播间 {room} 未开播或获取失败: {msg}"));
            }
            parse_bili_durl(&v2)
                .ok_or_else(|| format!("B站直播间 {room} 未找到可用的 FLV 流（可能未开播）"))
                .map(|u| (u, format!("https://live.bilibili.com/{room}")))
        }
    }
}

/// 从 getRoomPlayInfo 响应提取 http_stream / flv / avc 拉流地址（base_url + host）。
pub fn parse_bili_flv(v: &Value) -> Option<String> {
    let streams = v["data"]["playurl_info"]["playurl"]["stream"].as_array()?;
    for stream in streams {
        if stream["protocol_name"].as_str() != Some("http_stream") {
            continue;
        }
        for fmt in stream["format"].as_array()? {
            if fmt["format_name"].as_str() != Some("flv") {
                continue;
            }
            for codec in fmt["codec"].as_array()? {
                if codec["codec_name"].as_str() == Some("avc") {
                    let base = codec["base_url"].as_str()?;
                    let host = codec["url_list"].as_array()?.first()?.as_str()?;
                    return Some(format!("{host}{base}"));
                }
            }
        }
    }
    None
}

/// 从旧版 playUrl 响应提取 durl 直出的 FLV 地址。
pub fn parse_bili_durl(v: &Value) -> Option<String> {
    v["data"]["durl"]
        .as_array()?
        .iter()
        .find_map(|u| u["url"].as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

// ---------- FLV 解复用 / H.264 辅助（纯函数，可测） ----------

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

/// YUV420 平面（BT.601 有限色域）→ RGBA。
pub fn yuv420_to_rgba(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    y_stride: usize,
    uv_stride: usize,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let uv_row = row / 2;
        for col in 0..w {
            let yv = y[row * y_stride + col] as i32;
            let uv = u[uv_row * uv_stride + col / 2] as i32 - 128;
            let vv = v[uv_row * uv_stride + col / 2] as i32 - 128;
            let yy = yv - 16;
            let r = (1192 * yy + 1634 * vv) >> 10;
            let g = (1192 * yy - 400 * uv - 833 * vv) >> 10;
            let b = (1192 * yy + 2066 * uv) >> 10;
            out.push(r.clamp(0, 255) as u8);
            out.push(g.clamp(0, 255) as u8);
            out.push(b.clamp(0, 255) as u8);
            out.push(255);
        }
    }
    out
}

// ---------- 会话与帧总线 ----------

/// 解码产出的一帧。
struct Frame {
    seq: u64,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

/// 帧总线：小型回放缓冲（FIFO）。
/// 网络突发会让解码帧成簇到达——只显示最新帧会产生「冻结后跳帧」的卡顿。
/// 显示路径采用**目标深度弹出**：缓冲攒到 DISPLAY_BUFFER_TARGET 帧才按序弹出
/// （60Hz 轮询不会把缓冲立刻排空），稳定吸收约 3 帧的网络抖动；不足目标深度时
/// 复用队首帧。扫码路径始终取最新帧（抢码只关心当前画面）。
struct Frames {
    queue: std::collections::VecDeque<Frame>,
    /// 扫码消费者已消费的帧序号。
    scan_seen: u64,
}

/// 显示回放的目标缓冲深度（约 3 帧 ≈ 100~150ms 额外延迟）。
const DISPLAY_BUFFER_TARGET: usize = 4;
/// 队列硬上限（防极端突发撑爆内存；超出丢弃最旧帧）。
const FRAME_BUFFER_MAX: usize = 6;

struct LiveSession {
    stop: Arc<AtomicBool>,
    frames: Arc<Mutex<Frames>>,
    room: String,
}

static LIVE: Mutex<Option<LiveSession>> = Mutex::new(None);

fn lock_live() -> std::sync::MutexGuard<'static, Option<LiveSession>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 启动直播会话（解析流地址 + 启动解码线程，阻塞至拿到首帧或超时）。
pub async fn start(room: String) -> Result<(u32, u32), String> {
    stop();
    let (url, referer) = resolve_stream(&room).await?;
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(Mutex::new(Frames {
        queue: std::collections::VecDeque::new(),
        scan_seen: 0,
    }));
    let (url2, stop2, frames2) = (url.clone(), stop.clone(), frames.clone());
    std::thread::Builder::new()
        .name("live-decode".into())
        .spawn(move || decode_loop(url2, referer, stop2, frames2))
        .map_err(|e| format!("解码线程启动失败: {e}"))?;
    // 等首帧（流建立 + 关键帧到达，通常 1~4 秒）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        {
            let f = frames.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(back) = f.queue.back() {
                if back.w > 0 {
                    let (w, h) = (back.w, back.h);
                    drop(f);
                    *lock_live() = Some(LiveSession {
                        stop,
                        frames,
                        room: room.clone(),
                    });
                    return Ok((w, h));
                }
            }
        }
        if stop.load(Ordering::SeqCst) {
            return Err("直播会话已被取消".to_string());
        }
        if tokio::time::Instant::now() >= deadline {
            stop.store(true, Ordering::SeqCst);
            return Err(format!("B站直播间 {room} 连接超时（15 秒内未取到画面）"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 停止当前直播会话（断流 + 释放）。
pub fn stop() {
    if let Some(s) = lock_live().take() {
        s.stop.store(true, Ordering::SeqCst);
        log::info!("直播会话已断开（B站 {}）", s.room);
    }
}

/// 当前会话信息（前端日志用）。
pub fn session_label() -> Option<String> {
    let live = lock_live();
    live.as_ref().map(|s| format!("B站 {}", s.room))
}

/// 是否存在活跃会话。
pub fn active() -> bool {
    lock_live().is_some()
}

/// 扫码消费者：自上次消费以来有新帧时返回最新帧（全分辨率 RGB）。
pub fn take_scan_frame() -> Option<(Vec<u8>, u32, u32)> {
    let live = lock_live();
    let s = live.as_ref()?;
    let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
    let (seq, w, h) = {
        let back = f.queue.back()?;
        if back.seq == 0 || f.scan_seen == back.seq {
            return None;
        }
        (back.seq, back.w, back.h)
    };
    f.scan_seen = seq;
    let back = f.queue.back().unwrap();
    let rgb = crate::qr::rgba_to_rgb(&back.rgba);
    Some((rgb, w, h))
}

/// 预览消费者：按视口裁剪 + 面积降采样，组装 24 字节头 + RGBA 负载。
/// 头部布局与屏幕预览一致（mon = 流原始尺寸），无会话/无帧返回 None。
pub fn crop_frame(x: i32, y: i32, w: i32, h: i32, max_width: u32) -> Option<Vec<u8>> {
    let live = lock_live();
    let s = live.as_ref()?;
    let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
    if f.queue.is_empty() {
        return None;
    }
    // 攒到目标深度才按序弹出（不足时复用队首帧，60Hz 轮询不会排空缓冲）。
    let popped = if f.queue.len() >= DISPLAY_BUFFER_TARGET {
        f.queue.pop_front()
    } else {
        None
    };
    let f = match &popped {
        Some(fr) => fr,
        None => f.queue.front().unwrap(),
    };
    if f.w == 0 || f.h == 0 {
        return None;
    }
    let (fw, fh) = (f.w as i32, f.h as i32);
    let (cx, cy, cw, ch) = if w > 0 && h > 0 {
        let x0 = x.clamp(0, fw - 1);
        let y0 = y.clamp(0, fh - 1);
        let cw0 = (w.min(fw - x0)).max(1) as u32;
        let ch0 = (h.min(fh - y0)).max(1) as u32;
        (x0 as u32, y0 as u32, cw0, ch0)
    } else {
        (0, 0, f.w, f.h)
    };
    // 输出宽度不超过裁剪区实际像素数（禁上采样，避免黑网格）。
    let tw = max_width.min(cw).clamp(1, 4096);
    let th = (((ch as u64) * tw as u64) / cw as u64).max(1) as u32;
    let rgba = downsample_rgba(&f.rgba, f.w, (cx, cy, cw, ch), tw, th);
    let mut out = Vec::with_capacity(24 + rgba.len());
    out.extend_from_slice(&tw.to_le_bytes());
    out.extend_from_slice(&th.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&f.w.to_le_bytes());
    out.extend_from_slice(&f.h.to_le_bytes());
    out.extend_from_slice(&rgba);
    Some(out)
}

/// 面积平均降采样（RGBA 源，裁剪区 → tw×th），步进采样控 CPU。
fn downsample_rgba(
    src: &[u8],
    src_w: u32,
    crop: (u32, u32, u32, u32),
    tw: u32,
    th: u32,
) -> Vec<u8> {
    let (cx, cy, cw, ch) = (
        crop.0 as usize,
        crop.1 as usize,
        crop.2 as usize,
        crop.3 as usize,
    );
    let (tw, th) = (tw as usize, th as usize);
    let sw = src_w as usize;
    let x_step = cw.div_ceil(tw).max(1).div_ceil(2).max(1);
    let y_step = ch.div_ceil(th).max(1).div_ceil(2).max(1);
    let mut sums = vec![0u32; tw * th * 3];
    let mut counts = vec![0u16; tw * th];
    for sy in (0..ch).step_by(y_step) {
        let yb = sy * th / ch;
        for sx in (0..cw).step_by(x_step) {
            let xb = sx * tw / cw;
            let i = ((cy + sy) * sw + cx + sx) * 4;
            let o = (yb * tw + xb) * 3;
            sums[o] += src[i] as u32;
            sums[o + 1] += src[i + 1] as u32;
            sums[o + 2] += src[i + 2] as u32;
            counts[yb * tw + xb] += 1;
        }
    }
    let mut out = Vec::with_capacity(tw * th * 4);
    for i in 0..tw * th {
        let c = counts[i].max(1) as u32;
        out.push((sums[i * 3] / c) as u8);
        out.push((sums[i * 3 + 1] / c) as u8);
        out.push((sums[i * 3 + 2] / c) as u8);
        out.push(255);
    }
    out
}

// ---------- 解码线程 ----------

fn decode_loop(url: String, referer: String, stop: Arc<AtomicBool>, frames: Arc<Mutex<Frames>>) {
    let mut fails = 0u32;
    while !stop.load(Ordering::SeqCst) {
        match decode_session(&url, &referer, &stop, &frames) {
            Ok(()) => return, // 收到停止信号
            Err(e) => log::warn!("直播流断开: {e}"),
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        fails += 1;
        if fails > 20 {
            log::error!("直播流连续重连失败 20 次，解码线程退出");
            return;
        }
        log::info!("直播流重连（第 {fails} 次）…");
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// 流式读取 agent：只设连接/读超时（不设整体超时——拉流是长连接）。
fn stream_agent() -> &'static ureq::Agent {
    static A: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    A.get_or_init(|| {
        let connector = std::sync::Arc::new(
            native_tls::TlsConnector::builder()
                .build()
                .expect("TLS 初始化失败"),
        );
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .tls_connector(connector)
            .build()
    })
}

fn read_exact(reader: &mut impl Read, buf: &mut Vec<u8>, n: usize) -> Result<(), String> {
    buf.resize(n, 0);
    reader
        .read_exact(&mut buf[..n])
        .map_err(|_| "流已结束".to_string())
}

/// 一次拉流会话：连接 → FLV 逐 Tag 解复用 → H.264 解码 → 写共享帧。
/// 返回 Ok(()) 表示因停止信号退出；Err 表示连接/解析异常（上层重连）。
fn decode_session(
    url: &str,
    referer: &str,
    stop: &AtomicBool,
    frames: &Arc<Mutex<Frames>>,
) -> Result<(), String> {
    let req = stream_agent()
        .get(url)
        .set("User-Agent", BROWSER_UA)
        .set("Referer", referer);
    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {code}")),
        Err(e) => return Err(format!("{e}")),
    };
    let mut reader = resp.into_reader();

    // FLV 头（9 字节）+ PreviousTagSize0（4 字节）。
    let mut hdr = Vec::new();
    read_exact(&mut reader, &mut hdr, 13)?;
    if &hdr[0..3] != b"FLV" {
        return Err("响应不是 FLV 流".to_string());
    }

    let mut decoder =
        openh264::decoder::Decoder::new().map_err(|e| format!("解码器初始化失败: {e}"))?;
    let mut param_sets: Vec<u8> = Vec::new(); // SPS/PPS（Annex-B）
    let mut tag = Vec::new();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        // Tag 头 11 字节：type(1) data_size(3) timestamp(3+1) stream_id(3)。
        read_exact(&mut reader, &mut tag, 11)?;
        let tag_type = tag[0];
        let data_size = ((tag[1] as usize) << 16) | ((tag[2] as usize) << 8) | tag[3] as usize;
        if data_size > 8 * 1024 * 1024 {
            return Err("FLV Tag 超过大小上限".to_string());
        }
        read_exact(&mut reader, &mut tag, data_size + 4)?; // 体 + PreviousTagSize
        if tag_type != 9 || data_size < 5 {
            continue; // 只关心视频 Tag
        }
        let body = &tag[..data_size];
        let first = body[0];
        let _frame_type = first >> 4;
        if first & 0x0F != 7 {
            continue; // 非 AVC
        }
        match body[1] {
            0 => {
                // AVC sequence header：AVCDecoderConfigurationRecord → SPS/PPS。
                param_sets = avcc_config_to_annexb(&body[5..]);
                if !param_sets.is_empty() {
                    let _ = decoder.decode(&param_sets);
                }
            }
            1 => {
                let mut annexb = Vec::with_capacity(body.len() + 16);
                if first >> 4 == 1 && !param_sets.is_empty() {
                    // 关键帧前带参数集，保证解码器随时可恢复。
                    annexb.extend_from_slice(&param_sets);
                }
                // 体布局：flags(1) + AVCPacketType(1) + CompositionTime(3) + AVCC NALU。
                annexb.extend_from_slice(&avcc_to_annexb(&body[5..]));
                if annexb.is_empty() {
                    continue;
                }
                // 组合时间（body[2..5]）忽略——显示延迟可接受。
                if let Ok(Some(yuv)) = decoder.decode(&annexb) {
                    let (w, h) = yuv.dimensions();
                    if w == 0 || h == 0 {
                        continue;
                    }
                    let strides = yuv.strides();
                    let rgba = yuv420_to_rgba(
                        yuv.y(),
                        yuv.u(),
                        yuv.v(),
                        strides.0,
                        strides.1,
                        w as u32,
                        h as u32,
                    );
                    let mut f = frames.lock().unwrap_or_else(PoisonError::into_inner);
                    let seq = f.queue.back().map(|b| b.seq + 1).unwrap_or(1);
                    f.queue.push_back(Frame {
                        seq,
                        w: w as u32,
                        h: h as u32,
                        rgba,
                    });
                    while f.queue.len() > FRAME_BUFFER_MAX {
                        f.queue.pop_front();
                    }
                }
            }
            _ => {}
        }
    }
}
