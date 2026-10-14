//! 直播间「开播/下播」即时监测：B站弹幕 WebSocket。
//!
//! 背景：元数据轮询（30s 一轮）只能在事后发现开播/下播，感知延迟可达半分钟。
//! B站弹幕服务在房间状态切换时**实时推送** `LIVE`（开播）/`PREPARING`（下播）
//! 命令——每个直播间保持一条轻量 WS 连接，即可把感知延迟从「≤30s」降到「≈1s」。
//!
//! 链路（2026-10-14 先用 Python 原型对真实直播间全流程验证，再移植到本模块）：
//!   ① `live::fetch_meta`（get_info）→ 真实 room_id + 初始状态（兼作重连对账）；
//!   ② `getDanmuInfo`（**必须 wbi 签名**；实测免签名 / 仅 buvid 均被 -352 风控）
//!      → token + host_list；
//!   ③ `wss://{host}:{wss_port}/sub`：手写 WebSocket 握手（复用
//!      `http::stream_tls_connect` 的 rustls→schannel 回退）→ 认证包（op=7；
//!      16 字节头**含 sequence 字段**——少写 4 字节会被服务端 0.2s 内断连）→
//!      心跳（op=2，每 25s，body `[object Object]`）；
//!   ④ 解析下行包：op=5（protover=2，ver=2 批次 zlib 解压；ver=3 brotli 未申请、
//!      忽略），扫描 `"cmd":"LIVE"` / `"cmd":"PREPARING"` → 回调上报。
//!
//! 失败策略：断线按 5s→120s 指数退避重连；每轮重连环路先 get_info 对账一次，
//! 漏掉的状态变化会在重连时补报。抖音不做 WS（protobuf + 签名链路成本高），
//! 仍由轮询兜底（`sync` 会过滤非 B站房间）。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::http::ReadWrite;

/// 与外层 `fetch_bili_meta` 保持一致的浏览器 UA（与 Python 原型验证所用一致）。
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// 心跳间隔（服务端约 60s 无心跳判死；25s 留足余量）。
const HEARTBEAT: Duration = Duration::from_secs(25);
/// 静默上限：超过则该连接作废重连（正常时每 25s 至少有一次心跳回应）。
const SILENCE_LIMIT: Duration = Duration::from_secs(75);
/// WS 读超时（每次 read 的系统级超时；超时返回 WouldBlock 由主循环消化）。
const READ_TIMEOUT: Duration = Duration::from_secs(12);

// ---------- WBI 签名（getDanmuInfo 必需） ----------

/// WBI mixin key 的置换表（社区文档固定表，64 项）。
const MIXIN_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// wbi mixin key 缓存（6 小时 TTL；nav 接口的密钥每日轮换）。
static WBI_CACHE: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 取 wbi mixin key（带缓存）。
fn wbi_mixin() -> Result<String, String> {
    let cache = WBI_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((key, at)) = guard.as_ref() {
        if at.elapsed() < Duration::from_secs(6 * 3600) {
            return Ok(key.clone());
        }
    }
    let headers = vec![
        ("User-Agent".to_string(), UA.to_string()),
        (
            "Referer".to_string(),
            "https://www.bilibili.com/".to_string(),
        ),
    ];
    let r = crate::http::request_sync(
        "GET",
        "https://api.bilibili.com/x/web-interface/nav",
        &headers,
        None,
    )?;
    let v: Value = serde_json::from_str(&r.body).map_err(|e| format!("nav 响应解析失败: {e}"))?;
    let stem = |u: &str| -> String {
        u.rsplit('/')
            .next()
            .unwrap_or("")
            .split('.')
            .next()
            .unwrap_or("")
            .to_string()
    };
    let img = stem(v["data"]["wbi_img"]["img_url"].as_str().unwrap_or(""));
    let sub = stem(v["data"]["wbi_img"]["sub_url"].as_str().unwrap_or(""));
    let raw = format!("{img}{sub}");
    if raw.len() < 64 {
        return Err("nav 未返回 wbi 密钥".to_string());
    }
    let mixin: String = MIXIN_TAB
        .iter()
        .take(32)
        .map(|&i| raw.as_bytes()[i] as char)
        .collect();
    *guard = Some((mixin.clone(), Instant::now()));
    Ok(mixin)
}

/// 对查询参数做 wbi 签名，返回带 `wts`/`w_rid` 的完整查询串。
///
/// 注：本处参数均为纯数字（id/type/wts），与社区实现一致**无需百分号编码**；
/// 若未来加入非 ASCII/特殊字符参数，需补 urlencode（签名与请求必须同串）。
fn wbi_sign(params: &[(&str, String)]) -> Result<String, String> {
    let mixin = wbi_mixin()?;
    let mut kv: Vec<(&str, String)> = params.to_vec();
    kv.push(("wts", now_secs().to_string()));
    kv.sort_by(|a, b| a.0.cmp(b.0));
    let query = kv
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let w_rid = crate::crypto::md5_hex(&format!("{query}{mixin}"));
    Ok(format!("{query}&w_rid={w_rid}"))
}

// ---------- 弹幕服务器信息 ----------

struct DanmuInfo {
    token: String,
    hosts: Vec<(String, u16)>,
}

fn fetch_danmu_info(room_id: &str) -> Result<DanmuInfo, String> {
    let qs = wbi_sign(&[("id", room_id.to_string()), ("type", "0".to_string())])?;
    let url = format!("https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?{qs}");
    let headers = vec![
        ("User-Agent".to_string(), UA.to_string()),
        (
            "Referer".to_string(),
            format!("https://live.bilibili.com/{room_id}"),
        ),
    ];
    let r = crate::http::request_sync("GET", &url, &headers, None)?;
    let v: Value =
        serde_json::from_str(&r.body).map_err(|e| format!("getDanmuInfo 解析失败: {e}"))?;
    let code = v["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return Err(format!("getDanmuInfo code={code}（风控或接口变更）"));
    }
    let token = v["data"]["token"].as_str().unwrap_or("").to_string();
    let hosts: Vec<(String, u16)> = v["data"]["host_list"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|h| {
                    Some((
                        h["host"].as_str()?.to_string(),
                        h["wss_port"].as_u64().unwrap_or(443) as u16,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if token.is_empty() || hosts.is_empty() {
        return Err("getDanmuInfo 数据缺失".to_string());
    }
    Ok(DanmuInfo { token, hosts })
}

// ---------- 协议包 ----------

/// 组装协议包：16 字节头（len + header_len=16 + ver + op + **sequence**）+ body。
fn pack(op: u32, ver: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + body.len());
    v.extend_from_slice(&((16 + body.len()) as u32).to_be_bytes());
    v.extend_from_slice(&16u16.to_be_bytes());
    v.extend_from_slice(&ver.to_be_bytes());
    v.extend_from_slice(&op.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// 在一段（未压缩的）JSON 批次里扫描状态命令：1=开播（LIVE）、0=下播（PREPARING）。
///
/// 用「定位 `"cmd":"` 后取到下一个引号」的方式而非整包 JSON 解析——弹幕量大，
/// 全量解析不值得；转义引号（`\"cmd\":\"`）不会命中字面量模式，无误报风险。
fn scan_body(body: &[u8], out: &mut Vec<i64>) {
    const PAT: &[u8] = b"\"cmd\":\"";
    let mut i = 0usize;
    while let Some(p) = find(&body[i..], PAT) {
        let start = i + p + PAT.len();
        let end = body[start..]
            .iter()
            .position(|&b| b == b'"')
            .map(|e| start + e)
            .unwrap_or(start);
        match &body[start..end] {
            b"LIVE" => out.push(1),
            b"PREPARING" => out.push(0),
            _ => {}
        }
        i = end.max(start + 1);
        if i >= body.len() {
            break;
        }
    }
}

/// 解析一批协议包（服务端可能把多个包拼接压缩在一个 op=5 里，ver=2 为 zlib）。
fn parse_batch(data: &[u8], out: &mut Vec<i64>, auth_code: &mut Option<i64>, depth: u8) {
    if depth > 3 {
        return;
    }
    let mut pos = 0usize;
    while pos + 16 <= data.len() {
        let plen =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let hlen = u16::from_be_bytes([data[pos + 4], data[pos + 5]]) as usize;
        let ver = u16::from_be_bytes([data[pos + 6], data[pos + 7]]);
        let op = u32::from_be_bytes([data[pos + 8], data[pos + 9], data[pos + 10], data[pos + 11]]);
        if hlen < 16 || plen < hlen || pos + plen > data.len() {
            break;
        }
        let body = &data[pos + hlen..pos + plen];
        match op {
            // 普通消息
            5 => match ver {
                2 => {
                    if let Ok(inf) = miniz_oxide::inflate::decompress_to_vec_zlib(body) {
                        parse_batch(&inf, out, auth_code, depth + 1);
                    }
                }
                // brotli（protover=3）：本实现未申请，出现则忽略
                3 => {}
                _ => scan_body(body, out),
            },
            // 认证回应：{"code":0}
            8 => {
                if let Ok(v) = serde_json::from_slice::<Value>(body) {
                    *auth_code = v["code"].as_i64();
                }
            }
            _ => {}
        }
        pos += plen;
    }
}

// ---------- WebSocket（手写客户端） ----------

struct WsConn {
    stream: Box<dyn ReadWrite>,
    /// 已读入未消费的字节（握手多读 / 帧间残留）。
    buf: Vec<u8>,
}

impl WsConn {
    fn new(stream: Box<dyn ReadWrite>, leftover: Vec<u8>) -> Self {
        WsConn {
            stream,
            buf: leftover,
        }
    }

    fn fill(&mut self, need: usize) -> std::io::Result<()> {
        while self.buf.len() < need {
            let mut tmp = [0u8; 8192];
            let n = self.stream.read(&mut tmp)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "WS 连接关闭",
                ));
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
        Ok(())
    }

    /// 读一个完整消息帧；payload 写入 `out`，返回 opcode。读超时返回 WouldBlock。
    fn read_message(&mut self, out: &mut Vec<u8>) -> std::io::Result<u8> {
        self.fill(2)?;
        let opcode = self.buf[0] & 0x0f;
        let masked = self.buf[1] & 0x80 != 0;
        let mut len = (self.buf[1] & 0x7f) as usize;
        let mut pos = 2usize;
        if len == 126 {
            self.fill(4)?;
            len = u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize;
            pos = 4;
        } else if len == 127 {
            self.fill(10)?;
            let mut b = [0u8; 8];
            b.copy_from_slice(&self.buf[2..10]);
            len = u64::from_be_bytes(b) as usize;
            pos = 10;
        }
        let mask_len = if masked { 4 } else { 0 };
        // 防御：单帧上限（正常弹幕帧 <100KB；异常内容直接断链重连）。
        if len > 4 * 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "WS 帧过大",
            ));
        }
        self.fill(pos + mask_len + len)?;
        out.clear();
        out.extend_from_slice(&self.buf[pos + mask_len..pos + mask_len + len]);
        if masked {
            let mask: [u8; 4] = self.buf[pos..pos + 4].try_into().unwrap_or([0; 4]);
            for (i, byte) in out.iter_mut().enumerate() {
                *byte ^= mask[i & 3];
            }
        }
        self.buf.drain(..pos + mask_len + len);
        Ok(opcode)
    }

    /// 发送一个客户端帧（必须掩码）。
    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
        use rand::RngCore;
        let mut mask = [0u8; 4];
        rand::thread_rng().fill_bytes(&mut mask);
        let mut hdr = Vec::with_capacity(14);
        hdr.push(0x80 | opcode);
        if payload.len() < 126 {
            hdr.push(0x80 | payload.len() as u8);
        } else if payload.len() <= 0xffff {
            hdr.push(0x80 | 126);
            hdr.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            hdr.push(0x80 | 127);
            hdr.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        hdr.extend_from_slice(&mask);
        let masked: Vec<u8> = payload
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i & 3])
            .collect();
        self.stream.write_all(&hdr)?;
        self.stream.write_all(&masked)?;
        self.stream.flush()
    }
}

/// WebSocket 握手（客户端）。成功返回「握手响应之后多读入的字节」（可能含首帧）。
fn ws_handshake(stream: &mut Box<dyn ReadWrite>, host: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    use rand::RngCore;
    let mut key_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key_bytes);
    let key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
    let req = format!(
        "GET /sub HTTP/1.1\r\n\
         Host: {host}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Origin: https://live.bilibili.com\r\n\
         User-Agent: {UA}\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("WS 握手写出失败: {e}"))?;
    let _ = stream.flush();
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 2048];
    loop {
        let n = stream
            .read(&mut tmp)
            .map_err(|e| format!("WS 握手读失败: {e}"))?;
        if n == 0 {
            return Err("WS 握手期间连接被关闭".to_string());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]);
            if !head.starts_with("HTTP/1.1 101") {
                let status = head.lines().next().unwrap_or("(空)").to_string();
                return Err(format!("WS 握手被拒: {status}"));
            }
            return Ok(buf[p + 4..].to_vec());
        }
        if buf.len() > 16 * 1024 {
            return Err("WS 握手响应头过大".to_string());
        }
    }
}

// ---------- 会话与监测线程 ----------

/// 状态上报回调（room, 1=开播 / 0=下播）；允许在任意线程调用。
type StatusReport = dyn Fn(&str, i64) + Send + Sync;

/// 一次会话（直到断开/出错/被停止）。回调在**状态变化**与**每轮连接建立后**上报。
fn run_session(room: &str, stop: &AtomicBool, report: &StatusReport) -> Result<(), String> {
    // ① get_info：真实 room_id + 初始状态（重连时顺带对账补报）。
    let meta = crate::live::fetch_meta(crate::live::Platform::Bilibili, room);
    if meta.room_id.is_empty() {
        return Err("get_info 未返回 room_id".to_string());
    }
    report(room, meta.live_status);
    // ② getDanmuInfo（wbi 签名）
    let di = fetch_danmu_info(&meta.room_id)?;
    // ③ 逐节点尝试连接
    let mut conn: Option<WsConn> = None;
    let mut last_err = String::new();
    for (host, port) in &di.hosts {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match crate::http::stream_tls_connect(host, *port, READ_TIMEOUT) {
            Ok(mut stream) => match ws_handshake(&mut stream, host) {
                Ok(leftover) => {
                    conn = Some(WsConn::new(stream, leftover));
                    break;
                }
                Err(e) => last_err = format!("{host}:{port} {e}"),
            },
            Err(e) => last_err = format!("{host}:{port} {e}"),
        }
    }
    let Some(mut conn) = conn else {
        return Err(format!("全部节点连接失败（{last_err}）"));
    };
    // 认证包（op=7；roomid 用真实房间号）
    let roomid: Value = meta
        .room_id
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::String(meta.room_id.clone()));
    let auth = serde_json::json!({
        "uid": 0, "roomid": roomid, "protover": 2, "platform": "web", "type": 2,
        "key": di.token,
    });
    conn.send_frame(0x2, &pack(7, 1, auth.to_string().as_bytes()))
        .map_err(|e| format!("认证包发送失败: {e}"))?;

    // ④ 主循环：读包 / 心跳 / 静默重连 / 停止检查
    let mut last_rx = Instant::now();
    let mut last_hb = Instant::now();
    let mut last_status: Option<i64> = None;
    let mut payload: Vec<u8> = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = conn.send_frame(0x8, &[]); // close
            return Ok(());
        }
        if last_hb.elapsed() >= HEARTBEAT {
            conn.send_frame(0x2, &pack(2, 1, b"[object Object]"))
                .map_err(|e| format!("心跳发送失败: {e}"))?;
            last_hb = Instant::now();
        }
        match conn.read_message(&mut payload) {
            Ok(opcode) => {
                last_rx = Instant::now();
                match opcode {
                    0x8 => return Err("服务端主动关闭".to_string()),
                    0x9 => {
                        let _ = conn.send_frame(0xA, &payload);
                    }
                    0x0..=0x2 => {
                        let mut ops = Vec::new();
                        let mut auth_code = None;
                        parse_batch(&payload, &mut ops, &mut auth_code, 0);
                        if let Some(code) = auth_code {
                            if code != 0 {
                                return Err(format!("认证失败 code={code}"));
                            }
                        }
                        for st in ops {
                            if last_status != Some(st) {
                                last_status = Some(st);
                                report(room, st);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if last_rx.elapsed() > SILENCE_LIMIT {
                    return Err("静默超时（无任何入站数据）".to_string());
                }
            }
            Err(e) => return Err(format!("读取失败: {e}")),
        }
    }
}

/// 监测器注册表：key = "bilibili:{room}" → 停止标志。
static WATCHERS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();

/// 同步监测列表（幂等；前端在房间列表变化时调用）。
///
/// `report(room, live_status)`：1=开播、0=下播；线程安全，允许在任意线程调用。
pub fn sync(rooms: Vec<(String, String)>, report: impl Fn(&str, i64) + Send + Sync + 'static) {
    let map = WATCHERS.get_or_init(|| Mutex::new(HashMap::new()));
    let report: Arc<StatusReport> = Arc::new(report);
    let desired: HashMap<String, String> = rooms
        .into_iter()
        .filter(|(p, _)| p == "bilibili")
        .map(|(_, room)| (format!("bilibili:{room}"), room))
        .collect();
    let mut guard = map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // 停掉不再需要的房间
    guard.retain(|key, flag| {
        if desired.contains_key(key) {
            true
        } else {
            flag.store(true, Ordering::Relaxed);
            false
        }
    });
    // 启动新增的房间
    for (key, room) in desired {
        if guard.contains_key(&key) {
            continue;
        }
        let stop = Arc::new(AtomicBool::new(false));
        guard.insert(key, stop.clone());
        let report = report.clone();
        let room_for_err = room.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("live-watch-{room}"))
            .spawn(move || {
                let mut backoff = Duration::from_secs(5);
                while !stop.load(Ordering::Relaxed) {
                    match run_session(&room, &stop, &*report) {
                        Ok(()) => break, // stop 触发的正常退出
                        Err(e) => {
                            log::info!("直播间监听 {room} 断开：{e}（{backoff:?} 后重连）");
                            // 可中断退避（250ms 粒度响应 stop）
                            let until = Instant::now() + backoff;
                            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                                std::thread::sleep(Duration::from_millis(250));
                            }
                            backoff = (backoff * 2).min(Duration::from_secs(120));
                        }
                    }
                }
            });
        if spawned.is_err() {
            log::error!("直播间监听线程创建失败: {room_for_err}");
        }
    }
}

/// 诊断（探针测试用）：对单房间跑一次完整链路观察，返回摘要文本。
///
/// 与正式会话共用全部原语（wbi/getDanmuInfo/握手/认证/解析），只是有固定截止时间。
pub fn selftest(room: &str, seconds: u64) -> String {
    let meta = crate::live::fetch_meta(crate::live::Platform::Bilibili, room);
    if meta.room_id.is_empty() {
        return format!("[{room}] get_info 失败");
    }
    let di = match fetch_danmu_info(&meta.room_id) {
        Ok(d) => d,
        Err(e) => return format!("[{room}] {e}"),
    };
    let mut conn = None;
    for (host, port) in &di.hosts {
        if let Ok(mut stream) = crate::http::stream_tls_connect(host, *port, READ_TIMEOUT) {
            if let Ok(leftover) = ws_handshake(&mut stream, host) {
                conn = Some((WsConn::new(stream, leftover), host.clone()));
                break;
            }
        }
    }
    let Some((mut conn, host)) = conn else {
        return format!("[{room}] 全部节点连接失败");
    };
    let auth = serde_json::json!({
        "uid": 0, "roomid": meta.room_id.parse::<i64>().unwrap_or(0),
        "protover": 2, "platform": "web", "type": 2, "key": di.token,
    });
    if let Err(e) = conn.send_frame(0x2, &pack(7, 1, auth.to_string().as_bytes())) {
        return format!("[{room}] 认证发送失败: {e}");
    }
    let t0 = Instant::now();
    let mut last_hb = Instant::now();
    let (mut msgs, mut hb_replies) = (0u32, 0u32);
    let mut ops_seen: Vec<i64> = Vec::new();
    let mut payload = Vec::new();
    while t0.elapsed() < Duration::from_secs(seconds) {
        if last_hb.elapsed() >= HEARTBEAT {
            let _ = conn.send_frame(0x2, &pack(2, 1, b"[object Object]"));
            last_hb = Instant::now();
        }
        match conn.read_message(&mut payload) {
            Ok(0x0..=0x2) => {
                msgs += 1;
                let mut ops = Vec::new();
                let mut ac = None;
                parse_batch(&payload, &mut ops, &mut ac, 0);
                if ws_contains_heartbeat(&payload) {
                    hb_replies += 1;
                }
                ops_seen.extend(ops);
            }
            Ok(_) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return format!("[{room}] 读取失败: {e}"),
        }
    }
    format!(
        "[{room}] room_id={} status={} host={host} 观察 {seconds}s：消息 {msgs} 条、心跳回应 {hb_replies} 次、状态事件 {ops_seen:?}",
        meta.room_id, meta.live_status
    )
}

/// 诊断辅助：判断一个入站包中是否含 op=3（心跳回应，body 为 4 字节 popularity）。
fn ws_contains_heartbeat(data: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos + 16 <= data.len() {
        let plen =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let op = u32::from_be_bytes([data[pos + 8], data[pos + 9], data[pos + 10], data[pos + 11]]);
        if plen < 16 || pos + plen > data.len() {
            break;
        }
        if op == 3 {
            return true;
        }
        pos += plen;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 包头必须是 16 字节且**含 sequence 字段**——少写 4 字节会被服务端秒断
    /// （2026-10-14 实测：错版包头 0.2s 内被断连，修正后认证立即通过）。
    #[test]
    fn pack_header_is_16_bytes_with_sequence() {
        let p = pack(2, 1, b"ab");
        assert_eq!(p.len(), 18);
        assert_eq!(&p[0..4], &18u32.to_be_bytes()); // 总长 = 头 + body
        assert_eq!(&p[4..6], &16u16.to_be_bytes()); // 头长
        assert_eq!(&p[6..8], &1u16.to_be_bytes()); // ver
        assert_eq!(&p[8..12], &2u32.to_be_bytes()); // op
        assert_eq!(&p[12..16], &1u32.to_be_bytes()); // sequence
    }

    #[test]
    fn scan_body_detects_live_and_preparing() {
        let mut out = Vec::new();
        scan_body(br#"{"cmd":"LIVE","live_time":1697000000}"#, &mut out);
        assert_eq!(out, vec![1]);
        out.clear();
        scan_body(br#"{"cmd":"PREPARING","round":1}"#, &mut out);
        assert_eq!(out, vec![0]);
        out.clear();
        // 转义形式（弹幕文本里出现该字面量）不应误报。
        scan_body(
            br#"{"cmd":"DANMU_MSG","info":[[],"\"cmd\":\"LIVE\""]}"#,
            &mut out,
        );
        assert!(out.is_empty());
        out.clear();
        // 同一批次中多个命令连续匹配。
        scan_body(br#"{"cmd":"LIVE"}xx{"cmd":"PREPARING"}"#, &mut out);
        assert_eq!(out, vec![1, 0]);
    }

    #[test]
    fn parse_batch_plain_and_zlib() {
        let json = br#"{"cmd":"PREPARING"}"#;
        let mut ops = Vec::new();
        let mut ac = None;
        // 明文（ver=0）
        parse_batch(&pack(5, 0, json), &mut ops, &mut ac, 0);
        assert_eq!(ops, vec![0]);
        // zlib 批次（ver=2）：与「服务端把一个内层包压缩后发出」同构
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&pack(5, 0, json), 6);
        ops.clear();
        parse_batch(&pack(5, 2, &z), &mut ops, &mut ac, 0);
        assert_eq!(ops, vec![0]);
        // 认证回应（op=8）
        parse_batch(&pack(8, 0, br#"{"code":0}"#), &mut ops, &mut ac, 0);
        assert_eq!(ac, Some(0));
    }
}
