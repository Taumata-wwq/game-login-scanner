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

/// 同步版 B站流地址解析：解码线程重连时调用（每次拿全新 expires 的地址，
/// 自愈 CDN 临时 404 与地址过期——固定 URL 重连只会反复失败）。
fn resolve_stream_sync(room: &str) -> Result<(String, String), String> {
    let url = format!(
        "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={room}&protocol=0,1&format=0,2&codec=0,1&qn=10000&platform=web&ptype=16"
    );
    let referer_owned = format!("https://live.bilibili.com/{room}");
    let v: Value = stream_agent()
        .get(&url)
        .set("User-Agent", BROWSER_UA)
        .set("Referer", &referer_owned)
        .call()
        .map_err(|e| format!("B站直播接口请求失败: {e}"))?
        .into_string()
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .ok_or_else(|| "B站直播接口响应解析失败".to_string())?;
    if let Some(u) = parse_bili_flv(&v) {
        return Ok((u, referer_owned.clone()));
    }
    let real_room = v["data"]["room_id"]
        .as_i64()
        .map(|x| x.to_string())
        .unwrap_or_else(|| room.to_string());
    let url2 = format!(
        "https://api.live.bilibili.com/room/v1/Room/playUrl?cid={real_room}&qn=10000&platform=web"
    );
    let v2: Value = stream_agent()
        .get(&url2)
        .set("User-Agent", BROWSER_UA)
        .set("Referer", &referer_owned)
        .call()
        .map_err(|e| format!("B站直播接口请求失败: {e}"))?
        .into_string()
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .ok_or_else(|| "B站直播接口响应解析失败".to_string())?;
    if v2.get("code").and_then(|x| x.as_i64()) != Some(0) {
        return Err(format!("B站直播间 {room} 未开播或获取失败"));
    }
    parse_bili_durl(&v2)
        .ok_or_else(|| format!("B站直播间 {room} 未找到可用的 FLV 流（可能未开播）"))
        .map(|u| (u, format!("https://live.bilibili.com/{room}")))
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
/// 网络突发会让解码帧成簇到达——只显示最新帧会产生「冻结后跳帧」的卡顿，
/// 且「到达即弹出」会把网络抖动直接透传给显示（缓冲再深也无效）。
/// 显示路径采用**固定节拍弹出**：前端 30fps 轮询，每次轮询至多弹出一个最旧帧；
/// 显示间隔完全由轮询时钟决定（均匀 30fps），到达抖动被队列吸收。
/// 弹出速率(30) ≥ 到达速率(常见 24~30) 时延迟极小；到达更快时队列涨到
/// FRAME_BUFFER_MAX（≈200ms 缓冲）后均匀丢帧，显示仍是平滑 30fps。
/// 扫码路径始终取最新帧（抢码只关心当前画面）。
struct Frames {
    queue: std::collections::VecDeque<Frame>,
    /// 扫码消费者已消费的帧序号。
    scan_seen: u64,
}

/// 队列硬上限（≈200ms 显示缓冲；防极端突发撑爆内存，超出丢弃最旧帧）。
const FRAME_BUFFER_MAX: usize = 6;

/// 上次发送的预览帧标识：(seq, 裁剪区, 输出宽)。相同则返回短响应。
type SentKey = (u64, (u32, u32, u32, u32), u32);
static LAST_SENT: Mutex<Option<SentKey>> = Mutex::new(None);
/// 最近一次预览请求的视口（流坐标系）；扫码只分析该区域（与屏幕扫码
/// 「可视视口即扫描范围」语义一致，也大幅降低解码成本）。
static LAST_VIEW: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

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
    *LAST_SENT.lock().unwrap_or_else(PoisonError::into_inner) = None;
    // 初始校验：房间可解析、可开播（失败快速反馈给前端）；
    // 实际拉流地址由解码线程每次连接/重连时重新获取（新 expires，自愈临时故障）。
    resolve_stream(&room).await?;
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(Mutex::new(Frames {
        queue: std::collections::VecDeque::new(),
        scan_seen: 0,
    }));
    let (room2, stop2, frames2) = (room.clone(), stop.clone(), frames.clone());
    std::thread::Builder::new()
        .name("live-decode".into())
        .spawn(move || decode_loop(room2, stop2, frames2))
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

/// 是否存在活跃会话。
pub fn active() -> bool {
    lock_live().is_some()
}

/// RGBA 裁剪 + 2×2 面积平均 → RGB（供扫码：半尺寸即可承载游戏登录码，
/// 拷贝与解码成本约为全分辨率的 1/4）。
fn rgba_crop_half(rgba: &[u8], src_w: u32, crop: (u32, u32, u32, u32)) -> (Vec<u8>, u32, u32) {
    let (cx, cy, cw, ch) = crop;
    let (ow, oh) = (cw / 2, ch / 2);
    let mut out = Vec::with_capacity((ow as usize) * (oh as usize) * 3);
    for oy in 0..oh {
        for ox in 0..ow {
            let mut acc = [0u32; 3];
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = (((cy + oy * 2 + dy) as usize) * src_w as usize
                        + (cx + ox * 2 + dx) as usize)
                        * 4;
                    acc[0] += rgba[i] as u32;
                    acc[1] += rgba[i + 1] as u32;
                    acc[2] += rgba[i + 2] as u32;
                }
            }
            out.push((acc[0] / 4) as u8);
            out.push((acc[1] / 4) as u8);
            out.push((acc[2] / 4) as u8);
        }
    }
    (out, ow, oh)
}

/// 扫码消费者：自上次消费以来有新帧时返回最新帧的**当前显示视口**区域
/// （2×2 平均半尺寸 RGB）。视口未知时用全帧。
pub fn take_scan_frame() -> Option<(Vec<u8>, u32, u32)> {
    let live = lock_live();
    let s = live.as_ref()?;
    let view = *LAST_VIEW.lock().unwrap_or_else(PoisonError::into_inner);
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
    let crop = view.and_then(|(x, y, w, h)| {
        if w <= 0 || h <= 0 {
            return None;
        }
        let x0 = x.clamp(0, w - 1);
        let y0 = y.clamp(0, h - 1);
        let cw0 = (w.min(w - x0)).max(2) as u32;
        let ch0 = (h.min(h - y0)).max(2) as u32;
        Some((x0 as u32, y0 as u32, cw0, ch0))
    });
    let (rgb, w, h) = match crop {
        Some(c) => rgba_crop_half(&back.rgba, w, c),
        None => {
            let mut rgb = crate::qr::rgba_to_rgb(&back.rgba);
            let (w, h) = (w / 2, h / 2);
            // 全帧同样半尺寸，保持扫码成本恒定。
            downsample_rgb_half(&mut rgb, w * 2, h * 2, w, h);
            (rgb, w, h)
        }
    };
    Some((rgb, w, h))
}

/// RGB 2×2 面积平均（就地降采样到 ow×oh）。
fn downsample_rgb_half(rgb: &mut Vec<u8>, sw: u32, _sh: u32, ow: u32, oh: u32) {
    let src = std::mem::take(rgb);
    let sw = sw as usize;
    for oy in 0..oh as usize {
        for ox in 0..ow as usize {
            let mut acc = [0u32; 3];
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = ((oy * 2 + dy) * sw + ox * 2 + dx) * 3;
                    acc[0] += src[i] as u32;
                    acc[1] += src[i + 1] as u32;
                    acc[2] += src[i + 2] as u32;
                }
            }
            rgb.push((acc[0] / 4) as u8);
            rgb.push((acc[1] / 4) as u8);
            rgb.push((acc[2] / 4) as u8);
        }
    }
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

pub fn crop_frame(x: i32, y: i32, w: i32, h: i32, max_width: u32) -> FrameOut {
    *LAST_VIEW.lock().unwrap_or_else(PoisonError::into_inner) = if w > 0 && h > 0 {
        Some((x, y, w, h))
    } else {
        None
    };
    let live = lock_live();
    let s = match live.as_ref() {
        Some(s) => s,
        None => return FrameOut::NoSession,
    };
    let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
    if f.queue.is_empty() {
        return FrameOut::Empty;
    }
    // 弹出节拍 = 显示节拍（前端 30fps 轮询）：每轮询至多弹出一个最旧帧，
    // 队列空时复用队首帧。显示间隔完全由轮询时钟决定（均匀 30fps），到达抖动
    // 被缓冲吸收——「到达即弹出」会把抖动直接透传，缓冲再深也无济于事。
    let popped = f.queue.pop_front();
    let f = match &popped {
        Some(fr) => fr,
        None => f.queue.front().unwrap(),
    };
    if f.w == 0 || f.h == 0 {
        return FrameOut::Empty;
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
    // 帧未变（seq 相同）且视口/宽度未变：24 字节短响应，前端沿用上一帧
    // （复用屏幕预览协议），消除重复帧的降采样与 IPC。
    let key = (f.seq, (cx, cy, cw, ch), tw);
    {
        let mut last = LAST_SENT.lock().unwrap_or_else(PoisonError::into_inner);
        if *last == Some(key) {
            return FrameOut::Empty;
        }
        *last = Some(key);
    }
    let rgba = downsample_rgba(&f.rgba, f.w, (cx, cy, cw, ch), tw, th);
    let mut out = Vec::with_capacity(24 + rgba.len());
    out.extend_from_slice(&tw.to_le_bytes());
    out.extend_from_slice(&th.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&f.w.to_le_bytes());
    out.extend_from_slice(&f.h.to_le_bytes());
    out.extend_from_slice(&rgba);
    FrameOut::Frame(out)
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

fn decode_loop(room: String, stop: Arc<AtomicBool>, frames: Arc<Mutex<Frames>>) {
    let mut fails = 0u32;
    while !stop.load(Ordering::SeqCst) {
        // 每次连接/重连都重新解析流地址：CDN 临时 404 或地址过期时拿到全新地址自愈。
        let (url, referer) = match resolve_stream_sync(&room) {
            Ok(u) => u,
            Err(e) => {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                log::warn!("重连解析流地址失败: {e}");
                fails += 1;
                if fails > 4 {
                    log::error!("直播流连续重连失败，会话关闭（约 2 秒）");
                    remove_session_if(&frames);
                    return;
                }
                std::thread::sleep(Duration::from_millis(400));
                continue;
            }
        };
        match decode_session(&url, &referer, &stop, &frames) {
            Ok(()) => return, // 收到停止信号
            Err(e) => log::warn!("直播流断开: {e}"),
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        fails += 1;
        // 下播时 CDN 会快速关闭连接，重连也立即失败——快速放弃并移除会话，
        // 让预览/扫码自动回退屏幕（前端检测到空帧响应即切换）。
        if fails > 4 {
            log::error!("直播流连续重连失败，会话关闭（约 2 秒）");
            remove_session_if(&frames);
            return;
        }
        log::info!("直播流重连（第 {fails} 次）…");
        std::thread::sleep(Duration::from_millis(400));
    }
}

/// 仅当 LIVE 中仍是本线程对应的会话时移除（防止误删替换后的新会话）。
fn remove_session_if(frames: &Arc<Mutex<Frames>>) {
    let mut live = lock_live();
    if let Some(s) = live.as_ref() {
        if Arc::ptr_eq(&s.frames, frames) {
            live.take();
            log::info!("直播会话已关闭（流结束/重连失败），回退屏幕预览与扫码");
        }
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
