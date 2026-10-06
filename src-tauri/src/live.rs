//! 直播流捕获：B站/抖音 http-flv → FLV 解复用 → openh264 H.264 解码 → RGBA 帧总线。
//!
//! 低延迟链路：平台 API（免登录）解析出 http-flv 拉流地址 → 专用流式连接边下边解
//! （逐 Tag 解复用，解码即存共享帧），延迟主要由平台 CDN 决定（实测约 2~5 秒）。
//! 共享帧供两条消费路径：
//! - 预览：`crop_frame` 按前端视口裁剪降采样（等价屏幕预览协议）；
//! - 扫码：`take_scan_frame` 取新帧走既有二维码识别管线（seq 去重）。

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use openh264::formats::YUVSource;

use crate::http::header_utf8;

/// 直播平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Bilibili,
    Douyin,
}

pub fn parse_platform(s: &str) -> Option<Platform> {
    match s {
        "bilibili" => Some(Platform::Bilibili),
        "douyin" => Some(Platform::Douyin),
        _ => None,
    }
}

impl Platform {
    pub fn label(self) -> &'static str {
        match self {
            Platform::Bilibili => "B站",
            Platform::Douyin => "抖音",
        }
    }
}

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

// ---------- 流地址解析 ----------

/// 解析平台直播间为 http-flv 拉流地址，返回 (url, referer, cookie)。
async fn resolve_stream(
    platform: Platform,
    room: &str,
) -> Result<(String, String, Option<String>), String> {
    match platform {
        Platform::Bilibili => {
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
                return Ok((u, format!("https://live.bilibili.com/{room}"), None));
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
                .map(|u| (u, format!("https://live.bilibili.com/{room}"), None))
        }
        Platform::Douyin => {
            let ttwid = fetch_ttwid().await?;
            let headers = vec![
                ("User-Agent".to_string(), header_utf8(BROWSER_UA)),
                (
                    "Referer".to_string(),
                    format!("https://live.douyin.com/{room}"),
                ),
                ("Cookie".to_string(), format!("ttwid={ttwid}")),
            ];
            let url = format!(
                "https://live.douyin.com/webcast/room/web/enter/?aid=6383&device_platform=web&browser_language=zh-CN&browser_platform=Win32&browser_name=Chrome&browser_version=126.0.0.0&web_rid={room}"
            );
            let resp = crate::http::get(&url, headers)
                .await
                .map_err(|e| format!("抖音直播接口请求失败: {e}"))?;
            let v: Value = serde_json::from_str(&resp.body).map_err(|_| {
                format!(
                    "抖音直播接口响应非 JSON: {}",
                    &resp.body[..resp.body.len().min(120)]
                )
            })?;
            parse_douyin_flv(&v)
                .ok_or_else(|| {
                    format!(
                        "抖音直播间 {room} 未找到可用的 FLV 流（可能未开播、房间号有误或接口风控）"
                    )
                })
                .map(|u| {
                    (
                        u,
                        format!("https://live.douyin.com/{room}"),
                        Some(format!("ttwid={ttwid}")),
                    )
                })
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

/// ttwid 注册接口（抖音 Web API 的通行凭证，无需浏览器）。
async fn fetch_ttwid() -> Result<String, String> {
    let body = serde_json::json!({
        "region": "cn",
        "aid": 1768,
        "needFid": false,
        "service": "www.ixigua.com",
        "migrate_info": { "ticket": "", "source": "node" },
        "cbUrlProtocol": "https",
        "union": true,
    })
    .to_string();
    let resp = crate::http::post(
        "https://ttwid.bytedance.com/ttwid/room/register/",
        vec![("Content-Type".to_string(), "application/json".to_string())],
        &body,
    )
    .await
    .map_err(|e| format!("ttwid 获取失败: {e}"))?;
    for s in resp
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.as_str())
    {
        if let Some(rest) = s.trim().strip_prefix("ttwid=") {
            let val = rest.split(';').next().unwrap_or("").trim();
            if !val.is_empty() {
                return Ok(val.to_string());
            }
        }
    }
    Err("ttwid 获取失败（响应无 Cookie）".to_string())
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

/// 从抖音 enter 响应提取 flv 拉流地址（多代接口结构兼容，优先最高清晰度）。
pub fn parse_douyin_flv(v: &Value) -> Option<String> {
    let d0 = v.get("data")?.get("data")?.get(0)?;
    // 路径一：stream_url.flv_pull_url（清晰度名 → url）
    if let Some(obj) = d0["stream_url"]["flv_pull_url"].as_object() {
        for key in ["full_hd1", "hd1", "sd1", "sd2"] {
            if let Some(u) = obj.get(key).and_then(|x| x.as_str()) {
                if !u.is_empty() {
                    return Some(u.to_string());
                }
            }
        }
        if let Some((_, u)) = obj
            .iter()
            .find(|(_, v)| v.as_str().is_some_and(|s| !s.is_empty()))
        {
            return Some(u.as_str().unwrap_or_default().to_string());
        }
    }
    // 路径二：live_core_sdk_data.pull_data.stream_data（JSON 字符串）→ data.*.main.flv
    if let Some(s) = d0["stream_url"]["live_core_sdk_data"]["pull_data"]["stream_data"].as_str() {
        if let Ok(sd) = serde_json::from_str::<Value>(s) {
            if let Some(routes) = sd.get("data").and_then(|d| d.as_object()) {
                for route in routes.values() {
                    if let Some(u) = route["main"]["flv"].as_str() {
                        if !u.is_empty() {
                            return Some(u.to_string());
                        }
                    }
                }
            }
        }
    }
    None
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

/// 解码线程产出的最新一帧。
struct SharedFrame {
    seq: u64,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

struct LiveSession {
    stop: Arc<AtomicBool>,
    frame: Arc<Mutex<SharedFrame>>,
    /// 扫码消费者已消费的帧序号。
    scan_seen: AtomicU64,
    platform: Platform,
    room: String,
}

static LIVE: Mutex<Option<LiveSession>> = Mutex::new(None);

fn lock_live() -> std::sync::MutexGuard<'static, Option<LiveSession>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 启动直播会话（解析流地址 + 启动解码线程，阻塞至拿到首帧或超时）。
pub async fn start(platform: Platform, room: String) -> Result<(u32, u32), String> {
    stop();
    let (url, referer, cookie) = resolve_stream(platform, &room).await?;
    let stop = Arc::new(AtomicBool::new(false));
    let frame = Arc::new(Mutex::new(SharedFrame {
        seq: 0,
        w: 0,
        h: 0,
        rgba: Vec::new(),
    }));
    let (url2, stop2, frame2) = (url.clone(), stop.clone(), frame.clone());
    std::thread::Builder::new()
        .name("live-decode".into())
        .spawn(move || decode_loop(url2, referer, cookie, stop2, frame2))
        .map_err(|e| format!("解码线程启动失败: {e}"))?;
    // 等首帧（流建立 + 关键帧到达，通常 1~4 秒）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        {
            let f = frame.lock().unwrap_or_else(PoisonError::into_inner);
            if f.seq > 0 && f.w > 0 {
                let (w, h) = (f.w, f.h);
                drop(f);
                *lock_live() = Some(LiveSession {
                    stop,
                    frame,
                    scan_seen: AtomicU64::new(0),
                    platform,
                    room: room.clone(),
                });
                return Ok((w, h));
            }
        }
        if stop.load(Ordering::SeqCst) {
            return Err("直播会话已被取消".to_string());
        }
        if tokio::time::Instant::now() >= deadline {
            stop.store(true, Ordering::SeqCst);
            return Err(format!(
                "{}直播间 {room} 连接超时（15 秒内未取到画面）",
                platform.label()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 停止当前直播会话（断流 + 释放）。
pub fn stop() {
    if let Some(s) = lock_live().take() {
        s.stop.store(true, Ordering::SeqCst);
        log::info!("直播会话已断开（{} {}）", s.platform.label(), s.room);
    }
}

/// 当前会话信息（前端日志用）。
pub fn session_label() -> Option<String> {
    let live = lock_live();
    live.as_ref()
        .map(|s| format!("{} {}", s.platform.label(), s.room))
}

/// 是否存在活跃会话。
pub fn active() -> bool {
    lock_live().is_some()
}

/// 扫码消费者：取自上次消费以来的新帧（全分辨率 RGB）；无新帧返回 None。
pub fn take_scan_frame() -> Option<(Vec<u8>, u32, u32)> {
    let live = lock_live();
    let s = live.as_ref()?;
    let f = s.frame.lock().unwrap_or_else(PoisonError::into_inner);
    if f.seq == 0 || s.scan_seen.load(Ordering::SeqCst) == f.seq {
        return None;
    }
    s.scan_seen.store(f.seq, Ordering::SeqCst);
    let rgb = crate::qr::rgba_to_rgb(&f.rgba);
    Some((rgb, f.w, f.h))
}

/// 预览消费者：按视口裁剪 + 面积降采样，组装 24 字节头 + RGBA 负载。
/// 头部布局与屏幕预览一致（mon = 流原始尺寸），无会话/无帧返回 None。
pub fn crop_frame(x: i32, y: i32, w: i32, h: i32, max_width: u32) -> Option<Vec<u8>> {
    let live = lock_live();
    let s = live.as_ref()?;
    let f = s.frame.lock().unwrap_or_else(PoisonError::into_inner);
    if f.seq == 0 || f.w == 0 || f.h == 0 {
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

fn decode_loop(
    url: String,
    referer: String,
    cookie: Option<String>,
    stop: Arc<AtomicBool>,
    frame: Arc<Mutex<SharedFrame>>,
) {
    let mut fails = 0u32;
    while !stop.load(Ordering::SeqCst) {
        match decode_session(&url, &referer, cookie.as_deref(), &stop, &frame) {
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
    cookie: Option<&str>,
    stop: &AtomicBool,
    frame: &Arc<Mutex<SharedFrame>>,
) -> Result<(), String> {
    let mut req = stream_agent()
        .get(url)
        .set("User-Agent", BROWSER_UA)
        .set("Referer", referer);
    if let Some(c) = cookie {
        req = req.set("Cookie", c);
    }
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
                    let mut f = frame.lock().unwrap_or_else(PoisonError::into_inner);
                    f.seq += 1;
                    f.w = w as u32;
                    f.h = h as u32;
                    f.rgba = rgba;
                }
            }
            _ => {}
        }
    }
}
