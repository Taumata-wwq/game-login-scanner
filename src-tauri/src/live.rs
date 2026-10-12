//! 直播流捕获：B站 http-flv → FLV 解复用 → H.264 解码（MF 硬解，见 decode.rs）
//! → YUV 帧总线。
//!
//! 低延迟链路：B站 API（免登录）解析出 http-flv 拉流地址 → 专用流式连接边下边解
//! （逐 Tag 解复用，解码即存共享帧）。内置追帧机制（时间戳 vs 墙钟测积压，落后即丢弃
//! 至关键帧）；2026-10-11 起 MF 硬解（低延迟模式）+ 队列上限收紧（4 帧），
//! 端到端稳定在 0.3~0.4 秒量级。
//! 共享帧供两条消费路径：
//! - 预览：`crop_frame` 按前端视口裁剪降采样（等价屏幕预览协议）；
//! - 扫码：`take_scan_frame` 取新帧走既有二维码识别管线（seq 去重）。

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use crate::decode::{new_decoder, DecodedFrame, VideoDecoder};
use crate::http::header_utf8;

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

// ---------- 直播平台 ----------

/// 直播平台。B站：免登录 API + qn 档位降级；抖音：ttwid 三步解析（无档位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Bilibili,
    Douyin,
}

impl Platform {
    /// 从前端传入的字符串解析平台（默认 B站以兼容旧调用）。
    pub fn parse(s: &str) -> Platform {
        match s {
            "douyin" => Platform::Douyin,
            _ => Platform::Bilibili,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::Bilibili => "B站",
            Platform::Douyin => "抖音",
        }
    }

    /// 会话 key 用的平台标识（稳定、非展示用）。
    pub fn key(self) -> &'static str {
        match self {
            Platform::Bilibili => "bilibili",
            Platform::Douyin => "douyin",
        }
    }
}

/// B站流解析结果：候选 (url, referer) 列表 + 清晰度档位链。
type BiliStreamResult = (Vec<(String, String)>, Vec<u32>);

/// 解析出的流源：候选 (url, referer) 列表 + 可选 Cookie（抖音需 ttwid）。
pub struct StreamSource {
    pub candidates: Vec<(String, String)>,
    pub cookie: Option<String>,
    /// 房间实际提供的清晰度档位链（B站首次解析响应中顺带得到，避免二次 API
    /// 请求；抖音为 None）。降级重连用。
    pub qn_chain: Option<Vec<u32>>,
}

// ---------- 流地址解析 ----------

/// 解析 B站直播间为 http-flv 拉流地址，返回 (url, referer)。
async fn resolve_stream(room: &str, qn: u32) -> Result<(String, String), String> {
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
                "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={room}&protocol=0,1&format=0,2&codec=0,1&qn={qn}&platform=web&ptype=16"
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
            // 未开播快速失败（避免等 15s 首帧超时）。
            if let Some(ls) = v["data"]["live_status"].as_i64() {
                if ls != 1 {
                    return Err(format!("B站直播间 {room} 未开播"));
                }
            }
            // 首选：v2 结构化流（url_list 非空时）。
            if let Some(u) = parse_bili_flv(&v) {
                return Ok((u, format!("https://live.bilibili.com/{room}")));
            }
            // 无 FLV（HLS-only 房间）：回退 HLS。否则会走旧 playUrl 接口，
            // 该接口对 HLS-only 房间报「调用bvc-play-url-one出错」。
            if let Some(u) = crate::hls::parse_bili_hls(&v) {
                return Ok((u, format!("https://live.bilibili.com/{room}")));
            }
            // 回退：旧版 playUrl 接口 durl 直出完整 FLV 地址（免登录场景下
            // v2 的 url_list 常被服务端置空，旧接口反而稳定可用）。
            let real_room = v["data"]["room_id"]
                .as_i64()
                .map(|x| x.to_string())
                .unwrap_or_else(|| room.to_string());
            let url2 = format!(
                "https://api.live.bilibili.com/room/v1/Room/playUrl?cid={real_room}&qn={qn}&platform=web"
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
pub fn resolve_stream_sync(room: &str, qn: u32) -> Result<BiliStreamResult, String> {
    let url = format!(
        "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={room}&protocol=0,1&format=0,2&codec=0,1&qn={qn}&platform=web&ptype=16"
    );
    let referer_owned = format!("https://live.bilibili.com/{room}");
    let headers = vec![
        ("User-Agent".to_string(), BROWSER_UA.to_string()),
        ("Referer".to_string(), referer_owned.clone()),
    ];
    let body = crate::http::request_sync("GET", &url, &headers, None)
        .map_err(|e| format!("B站直播接口请求失败: {e}"))?
        .body;
    let v: Value = serde_json::from_str(&body)
        .ok()
        .ok_or_else(|| "B站直播接口响应解析失败".to_string())?;
    // 未开播快速失败：playurl 响应 data.live_status != 1 时无流，若继续走
    // durl 解析会拿到占位/失效地址，导致解码线程反复重连直到 15s 超时。
    if let Some(ls) = v["data"]["live_status"].as_i64() {
        if ls != 1 {
            return Err(format!("B站直播间 {room} 未开播"));
        }
    }
    // 顺带从同一响应解析清晰度档位链（避免 decode_loop 再发一次相同请求）。
    let qn_chain = build_qn_chain(&parse_accept_qn(&v));
    let flv_urls = parse_bili_flv_all(&v);
    if !flv_urls.is_empty() {
        // 多 CDN 节点全部收下（解码线程轮换重连；实测 TCP RTT 择优反而抬高
        // 首帧延迟，故不做测速排序，保持 API 原顺序）。
        let deduped = dedup_by_host(flv_urls);
        return Ok((
            deduped
                .into_iter()
                .map(|u| (u, referer_owned.clone()))
                .collect(),
            qn_chain,
        ));
    }
    // 无 FLV（部分房间服务端只提供 HLS）：回退 HLS（m3u8）。
    // decode_loop 按 URL 含 .m3u8 走 HLS 解码路径。
    if let Some(u) = crate::hls::parse_bili_hls(&v) {
        log::info!("房间 {room} 仅提供 HLS 流，使用 LL-HLS 拉流");
        return Ok((vec![(u, referer_owned.clone())], qn_chain));
    }
    let real_room = v["data"]["room_id"]
        .as_i64()
        .map(|x| x.to_string())
        .unwrap_or_else(|| room.to_string());
    let url2 = format!(
        "https://api.live.bilibili.com/room/v1/Room/playUrl?cid={real_room}&qn={qn}&platform=web"
    );
    let headers2 = vec![
        ("User-Agent".to_string(), BROWSER_UA.to_string()),
        ("Referer".to_string(), referer_owned.clone()),
    ];
    let body2 = crate::http::request_sync("GET", &url2, &headers2, None)
        .map_err(|e| format!("B站直播接口请求失败: {e}"))?
        .body;
    let v2: Value = serde_json::from_str(&body2)
        .ok()
        .ok_or_else(|| "B站直播接口响应解析失败".to_string())?;
    if v2.get("code").and_then(|x| x.as_i64()) != Some(0) {
        return Err(format!("B站直播间 {room} 未开播或获取失败"));
    }
    // durl 列表 = 多个 CDN 节点候选：固定用第一个可能撞上慢/故障节点，
    // 全部收下由解码线程轮换连接（慢流/断流自动换节点）。
    let durls = v2["data"]["durl"]
        .as_array()
        .ok_or_else(|| format!("B站直播间 {room} 未找到可用的 FLV 流（可能未开播）"))?;
    let referer = format!("https://live.bilibili.com/{room}");
    let raw: Vec<String> = durls
        .iter()
        .filter_map(|u| u["url"].as_str())
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
        .collect();
    if raw.is_empty() {
        return Err(format!(
            "B站直播间 {room} 未找到可用的 FLV 流（可能未开播）"
        ));
    }
    // 多 CDN 节点去重后全部保留（解码线程轮换重连）。
    let out: Vec<(String, String)> = dedup_by_host(raw)
        .into_iter()
        .map(|u| (u, referer.clone()))
        .collect();
    Ok((out, qn_chain))
}

/// 按平台解析流源（解码线程重连与探针共用）。返回候选列表 + 可选 Cookie。
pub fn resolve_source_sync(
    platform: Platform,
    room: &str,
    qn: u32,
) -> Result<StreamSource, String> {
    match platform {
        Platform::Bilibili => {
            resolve_stream_sync(room, qn).map(|(candidates, qn_chain)| StreamSource {
                candidates,
                cookie: None,
                qn_chain: Some(qn_chain),
            })
        }
        Platform::Douyin => {
            let (url, referer, cookie) = crate::douyin::resolve_sync(room)?;
            Ok(StreamSource {
                candidates: vec![(url, referer)],
                cookie: Some(cookie),
                qn_chain: None,
            })
        }
    }
}

/// 从 URL 中提取 CDN 主机名（节点去重用）。
fn host_of(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

/// 按 host 去重（保留首次出现顺序），避免同一节点重复出现在候选列表。
fn dedup_by_host(urls: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    urls.into_iter()
        .filter(|u| seen.insert(host_of(u).to_string()))
        .collect()
}

/// 从 playinfo 响应解析该房间**实际提供**的清晰度档位（降序）。
/// 关键：不同房间档位不同——有的仅原画 [10000]，有的多档 [10000,400,250,150]。
/// 降级链必须据此构建，否则请求不存在的档位会被 B站回退到原画（降级静默失效）。
pub fn parse_accept_qn(v: &Value) -> Vec<u32> {
    let Some(streams) = v["data"]["playurl_info"]["playurl"]["stream"].as_array() else {
        return Vec::new();
    };
    for stream in streams {
        if stream["protocol_name"].as_str() != Some("http_stream") {
            continue;
        }
        let Some(fmts) = stream["format"].as_array() else {
            continue;
        };
        for fmt in fmts {
            if fmt["format_name"].as_str() != Some("flv") {
                continue;
            }
            let Some(codecs) = fmt["codec"].as_array() else {
                continue;
            };
            for codec in codecs {
                if codec["codec_name"].as_str() == Some("avc") {
                    if let Some(arr) = codec["accept_qn"].as_array() {
                        let mut qs: Vec<u32> = arr
                            .iter()
                            .filter_map(|x| x.as_u64().map(|n| n as u32))
                            .collect();
                        qs.sort_unstable_by(|a, b| b.cmp(a));
                        qs.dedup();
                        return qs;
                    }
                }
            }
        }
    }
    Vec::new()
}

/// 从 codec 对象拼出可拉流的完整 URL。
///
/// B站 API 有新旧两种结构：
/// - 新：`url_info[{host, extra}]`——完整 URL = host + base_url + extra。
/// - 旧：`url_list[host]`——完整 URL = host + base_url。
///
/// 实测新结构下 url_list 为 null，旧代码只读 url_list 会解析失败。
pub fn codec_url(codec: &Value) -> Option<String> {
    codec_urls(codec).into_iter().next()
}

/// 从 codec 对象拼出**全部** CDN host 的完整 URL（同一房间通常返回 2 个及以上：
/// 区域 CT 节点 + B站 BGP `d1--cn-gotcha` 节点）。实测同房间两节点 TCP RTT
/// 可差 5 倍（10ms vs 55ms），只取第一个会错失低延迟节点。
pub fn codec_urls(codec: &Value) -> Vec<String> {
    let Some(base) = codec["base_url"].as_str() else {
        return Vec::new();
    };
    if let Some(infos) = codec["url_info"].as_array() {
        return infos
            .iter()
            .filter_map(|info| {
                let host = info["host"].as_str()?;
                let extra = info["extra"].as_str().unwrap_or("");
                Some(format!("{host}{base}{extra}"))
            })
            .collect();
    }
    codec["url_list"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|h| h.as_str())
                .map(|host| format!("{host}{base}"))
                .collect()
        })
        .unwrap_or_default()
}

/// 从 getRoomPlayInfo 响应提取 http_stream / flv / avc 拉流地址（base_url + host）。
pub fn parse_bili_flv(v: &Value) -> Option<String> {
    parse_bili_flv_all(v).into_iter().next()
}

/// 提取 http_stream / flv / avc 的**全部** CDN host 候选（多节点择优用）。
pub fn parse_bili_flv_all(v: &Value) -> Vec<String> {
    let Some(streams) = v["data"]["playurl_info"]["playurl"]["stream"].as_array() else {
        return Vec::new();
    };
    for stream in streams {
        if stream["protocol_name"].as_str() != Some("http_stream") {
            continue;
        }
        let Some(fmts) = stream["format"].as_array() else {
            continue;
        };
        for fmt in fmts {
            if fmt["format_name"].as_str() != Some("flv") {
                continue;
            }
            let Some(codecs) = fmt["codec"].as_array() else {
                continue;
            };
            for codec in codecs {
                if codec["codec_name"].as_str() == Some("avc") {
                    let urls = codec_urls(codec);
                    if !urls.is_empty() {
                        return urls;
                    }
                }
            }
        }
    }
    Vec::new()
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

// ---------- SPS VUI 清除（历史兼容保险） ----------

/// 从 offset 起查找 Annex-B 起始码（00 00 01 / 00 00 00 01），返回首个 0 的位置。
fn find_start_code(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 {
            if buf[i + 2] == 1 {
                return Some(i);
            }
            if i + 4 <= buf.len() && buf[i + 2] == 0 && buf[i + 3] == 1 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// EBSP（含 emulation prevention 0x03）→ RBSP。
fn ebsp_to_rbsp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0u32;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// RBSP → EBSP（插入 emulation prevention 0x03）。
fn rbsp_to_ebsp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 8);
    let mut zeros = 0u32;
    for &b in data {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// 极简 RBSP 比特读取器（仅 SPS 定位用）。
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn bit(&mut self) -> Option<u32> {
        if self.pos >= self.data.len() * 8 {
            return None;
        }
        let v = (self.data[self.pos / 8] >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(v as u32)
    }
    fn bits(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut z = 0u32;
        while self.bit()? == 0 {
            z += 1;
            if z > 31 {
                return None;
            }
        }
        Some((1u32 << z) - 1 + self.bits(z as usize).unwrap_or(0))
    }
    fn se(&mut self) -> Option<i32> {
        let v = self.ue()?;
        let mag = v.div_ceil(2) as i32;
        Some(if v % 2 == 1 { mag } else { -mag })
    }
}

/// 解析 SPS RBSP，返回 vui_parameters_present_flag 的比特位置。
fn find_sps_vui_flag_pos(rbsp: &[u8]) -> Option<usize> {
    let mut r = BitReader::new(rbsp);
    let profile = r.bits(8)?;
    r.bits(8)?; // constraint_set_flags + reserved_zero_2bits
    r.bits(8)?; // level_idc
    r.ue()?; // seq_parameter_set_id
    const HIGH_PROFILES: [u32; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];
    if HIGH_PROFILES.contains(&profile) {
        let chroma = r.ue()?;
        if chroma == 3 {
            r.bit()?; // separate_colour_plane_flag
        }
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.bit()?; // qpprime_y_zero_transform_bypass_flag
        if r.bit()? == 1 {
            let cnt = if chroma != 3 { 8 } else { 12 };
            for i in 0..cnt {
                if r.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let mut last = 8i32;
                    let mut next = 8i32;
                    for _ in 0..size {
                        if next != 0 {
                            let d = r.se()?;
                            next = (last + d + 256) % 256;
                        }
                        last = if next != 0 { next } else { last };
                    }
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            r.bit()?; // delta_pic_order_always_zero_flag
            r.se()?; // offset_for_non_ref_pic
            r.se()?; // offset_for_top_to_bottom_field
            let n = r.ue()?;
            for _ in 0..n {
                r.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?; // gaps_in_frame_num_value_allowed_flag
    r.ue()?; // pic_width_in_mbs_minus1
    r.ue()?; // pic_height_in_map_units_minus1
    if r.bit()? == 0 {
        r.bit()?; // mb_adaptive_frame_field_flag
    }
    r.bit()?; // direct_8x8_inference_flag
    if r.bit()? == 1 {
        for _ in 0..4 {
            r.ue()?; // frame_crop_*_offset
        }
    }
    Some(r.pos)
}

/// 清除 Annex-B 中**所有** SPS NAL 的 vui_parameters_present_flag。
///
/// 历史背景：原 openh264 软解对某些带 VUI（尤其 HRD 参数）的合法 SPS 解析失败
/// （dsNoParamSets，实测房间 11762420）；而 VUI 对解码/扫码无用。MF 硬解路径
/// 不需要此处理，但保留作为无害保险（仅翻转 flag 位、无数据重排；VUI 信息对
/// 本工具的消费端无价值），且 300 间直播间批量测试即在此行为下验证通过。
///
/// 必须遍历全部 NAL：sequence header 里的 SPS 之外，IDR 帧常内嵌一份重复 SPS，
/// 若只清参数集里的，IDR 内嵌那份仍可能触发解码侧解析问题。
/// 返回 (处理后的数据, 是否有 NAL 被修改)。
pub fn strip_sps_vui(annexb: &[u8]) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity(annexb.len());
    let mut changed = false;
    let mut i = 0usize;
    while i < annexb.len() {
        let Some(sc) = find_start_code(annexb, i) else {
            out.extend_from_slice(&annexb[i..]);
            break;
        };
        out.extend_from_slice(&annexb[i..sc]);
        let sc_len = if annexb[sc + 2] == 1 { 3 } else { 4 };
        let nal_start = sc + sc_len;
        let next = find_start_code(annexb, nal_start).unwrap_or(annexb.len());
        let nal = &annexb[nal_start..next];
        let mut handled = false;
        if nal.first() == Some(&0x67) {
            if let Some(rbsp) = nal.get(1..) {
                if let Some(pos) = find_sps_vui_flag_pos(&ebsp_to_rbsp(rbsp)) {
                    let mut rbsp = ebsp_to_rbsp(rbsp);
                    let (byte, bit) = (pos / 8, 7 - (pos % 8));
                    if byte < rbsp.len() && rbsp[byte] & (1 << bit) != 0 {
                        rbsp[byte] &= !(1 << bit);
                        out.extend_from_slice(&annexb[sc..nal_start]);
                        out.push(0x67);
                        out.extend_from_slice(&rbsp_to_ebsp(&rbsp));
                        changed = true;
                        handled = true;
                    }
                }
            }
        }
        if !handled {
            out.extend_from_slice(&annexb[sc..next]);
        }
        i = next;
    }
    (out, changed)
}

/// 紧凑 YUV420 裁剪区 → RGB（3 字节/像素），面积平均降采样到 tw×th。
///
/// **只转换输出像素所需的源区域**（对比早先「全帧转 RGBA 再降采样」），
/// 预览 320 宽时转换量约为全帧的 1/16。Y 取箱内平均，U/V 取对应色度箱平均。
/// `src_w` 为源帧宽（紧凑布局，Y 跨距 = src_w，UV 跨距 = src_w/2）。
#[allow(clippy::too_many_arguments)]
fn yuv_crop_to_rgb(
    y: &[u8],
    u: &[u8],
    v: &[u8],
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
    let cw2 = src_w / 2; // 色度跨距
    let mut out = Vec::with_capacity(tw * th * 3);
    for oy in 0..th {
        let sy0 = cy + oy * ch / th;
        let sy1 = (cy + (oy + 1) * ch / th).max(sy0 + 1);
        let cys0 = sy0 / 2;
        let cys1 = (sy1 / 2).max(cys0 + 1);
        for ox in 0..tw {
            let sx0 = cx + ox * cw / tw;
            let sx1 = (cx + (ox + 1) * cw / tw).max(sx0 + 1);
            // Y 箱步进平均（step=1 为全平均，扫码用；step>1 为采样，预览用）。
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
            // 色度箱步进平均（色度是 Y 的一半分辨率，step 相应处理）。
            let cxs0 = sx0 / 2;
            let cxs1 = (sx1 / 2).max(cxs0 + 1);
            let cstep = step.div_ceil(2).max(1);
            let mut us = 0u32;
            let mut vs = 0u32;
            let mut cn = 0u32;
            let mut cyy = cys0;
            while cyy < cys1 {
                let base = cyy * cw2;
                let mut cxx = cxs0;
                while cxx < cxs1 {
                    us += u[base + cxx] as u32;
                    vs += v[base + cxx] as u32;
                    cn += 1;
                    cxx += cstep;
                }
                cyy += cstep;
            }
            let yv = (ysum / yn.max(1)) as i32;
            let uv = (us / cn.max(1)) as i32 - 128;
            let vv = (vs / cn.max(1)) as i32 - 128;
            let yy = yv - 16;
            let r = (1192 * yy + 1634 * vv) >> 10;
            let g = (1192 * yy - 400 * uv - 833 * vv) >> 10;
            let b = (1192 * yy + 2066 * uv) >> 10;
            out.push(r.clamp(0, 255) as u8);
            out.push(g.clamp(0, 255) as u8);
            out.push(b.clamp(0, 255) as u8);
        }
    }
    out
}

/// 同 yuv_crop_to_rgb，但输出 RGBA（预览协议用，多一字节 255）。
#[allow(clippy::too_many_arguments)]
fn yuv_crop_to_rgba(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    src_w: usize,
    src_h: usize,
    crop: (u32, u32, u32, u32),
    tw: usize,
    th: usize,
    step: usize,
) -> Vec<u8> {
    let rgb = yuv_crop_to_rgb(y, u, v, src_w, src_h, crop, tw, th, step);
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.as_chunks::<3>().0 {
        out.extend_from_slice(&[px[0], px[1], px[2], 255]);
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
struct Frame {
    seq: u64,
    /// FLV 媒体时间戳（ms，流自身时钟）。输出侧追帧与诊断用。
    ts: u32,
    w: u32,
    h: u32,
    /// Y 平面，紧凑布局（行跨距 = w）。
    y: Vec<u8>,
    /// U 平面（w/2 × h/2，行跨距 = w/2）。
    u: Vec<u8>,
    /// V 平面（w/2 × h/2，行跨距 = w/2）。
    v: Vec<u8>,
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
struct Frames {
    queue: std::collections::VecDeque<Arc<Frame>>,
    /// 最新一帧（解码线程每次产出即更新；扫码专用，不受显示弹出影响）。
    latest: Option<Arc<Frame>>,
    /// 上次扫码输出的 RGB 缓存。无新帧时复用，保证扫码供帧率 100%——
    /// 低帧率流（如 ~2fps）不会因某轮 400ms 窗口内无新帧而「停一拍」
    /// （实测 100x178 流会丢 15%~23% 的采样轮次）。
    scan_cache: Option<ScanCache>,
    /// 单调递增的帧序号。**不能**用「队列尾 seq + 1」——队列空时那会导致
    /// 新帧 seq 恒为 1，使 LAST_SENT 去重把不同帧误判为同一帧（预览卡成 1 帧）。
    next_seq: u64,
}

/// 队列硬上限（≈133ms 显示缓冲；防极端突发撑爆内存，超出丢弃最旧帧）。
/// 2026-10-11：由 6 降至 4——MF 硬解吞吐充分（远快于流帧率），不再需要
/// 以深缓冲吸收解码抖动；队列深度直接叠加在端到端延迟上（每帧 16.7ms@60fps），
/// 减 2 帧即省 ~33~67ms 延迟。
const FRAME_BUFFER_MAX: usize = 4;

/// 扫码 RGB 缓存：键为 (帧序号, 裁剪区)，视口不变且无新帧时直接复用。
/// rgb 用 Arc：命中时只克隆指针（零拷贝），避免在持锁路径上复制整块图像。
struct ScanCache {
    seq: u64,
    crop: Option<(u32, u32, u32, u32)>,
    rgb: Arc<Vec<u8>>,
    w: u32,
    h: u32,
}

/// 上次发送的预览帧标识：(seq, 裁剪区, 输出宽)。相同则返回短响应。
type SentKey = (u64, (u32, u32, u32, u32), u32);

/// 单个直播会话（多路监控：每房间一个，独立解码线程 + 帧总线 + 预览/扫码状态）。
struct LiveSession {
    /// 会话标识 `{platform_key}:{room}`。
    id: String,
    stop: Arc<AtomicBool>,
    /// 后台暂停标志：true 时解码线程**停止读 socket**（TCP 背压 → CDN 停发
    /// → 零带宽零解码），切换回前台时恢复（无需重连，秒切）。这是多路监控
    /// 「后台不占资源」的核心机制。
    paused: Arc<AtomicBool>,
    frames: Arc<Mutex<Frames>>,
    room: String,
    platform: Platform,
    /// 最近一次预览请求的视口（流坐标系）；扫码只分析该区域（与屏幕扫码
    /// 「可视视口即扫描范围」语义一致）。按会话分桶，多路互不干扰。
    last_view: Mutex<Option<(i32, i32, i32, i32)>>,
    /// 上次发送的预览帧标识（按会话分桶）。
    last_sent: Mutex<Option<SentKey>>,
    /// 直播间元数据（标题/主播/封面等）。
    meta: Mutex<RoomMeta>,
}

/// 当前前台（主画面）会话 id。后台会话暂停拉流；切换时更新。
/// 扫码只扫该会话（避免后台画面误触确认）。
static ACTIVE_ID: Mutex<Option<String>> = Mutex::new(None);

fn set_active_id(id: Option<String>) {
    *ACTIVE_ID.lock().unwrap_or_else(PoisonError::into_inner) = id;
}

/// 当前前台会话 id（扫码用）。
pub fn active_id() -> Option<String> {
    ACTIVE_ID
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// 设置前台会话：恢复该会话拉流，暂停其余会话。id 为空则全部暂停。
/// 恢复的会话清空显示队列（丢弃暂停期间积压的旧帧），切换即显示最新画面。
pub fn set_active(id: &str) {
    set_active_id(Some(id.to_string()));
    let sessions = lock_sessions();
    if let Some(map) = sessions.as_ref() {
        for (k, s) in map {
            let was_paused = s.paused.swap(k != id, Ordering::SeqCst);
            // 从暂停恢复：清空显示队列与发送去重（避免旧帧快进）。
            if was_paused && k == id {
                let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
                f.queue.clear();
                f.scan_cache = None;
            }
        }
    }
}

/// 暂停全部会话（无前台时，如退出直播模式）。
pub fn pause_all() {
    set_active_id(None);
    let sessions = lock_sessions();
    if let Some(map) = sessions.as_ref() {
        for s in map.values() {
            s.paused.store(true, Ordering::SeqCst);
        }
    }
}

/// 前端房间列表用的会话摘要。
#[derive(Clone, serde::Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub room: String,
    pub platform: String,
    pub w: u32,
    pub h: u32,
    /// 直播间标题（元数据，可能为空）。
    #[serde(default)]
    pub title: String,
    /// 主播昵称（元数据，可能为空）。
    #[serde(default)]
    pub nickname: String,
    /// 封面图 URL（元数据，可能为空）。
    #[serde(default)]
    pub cover: String,
    /// 房间号（元数据；B站真实房间号可能不同于短号）。
    #[serde(default)]
    pub room_id: String,
    /// 已开播时长文本（如「1:23:45」，可能为空）。
    #[serde(default)]
    pub duration: String,
}

/// 直播间元数据（标题/主播/封面/房间号/开播状态/开播时刻）。
#[derive(Default, Clone)]
pub struct RoomMeta {
    pub title: String,
    pub nickname: String,
    pub cover: String,
    pub room_id: String,
    /// 1 = 直播中，0 = 未开播。
    pub live_status: i64,
    /// 开播时刻（Unix 秒）；0 = 未知/未开播。
    pub live_start: i64,
}

/// 按平台拉取直播间元数据（供前端轮询刷新用）。
pub fn fetch_meta(platform: Platform, room: &str) -> RoomMeta {
    match platform {
        Platform::Bilibili => fetch_bili_meta(room),
        Platform::Douyin => {
            let m = crate::douyin::fetch_meta_sync(room);
            RoomMeta {
                title: m.title,
                nickname: m.nickname,
                cover: m.cover,
                room_id: m.room_id,
                live_status: m.live_status,
                live_start: m.live_start,
            }
        }
    }
}

/// 开播时长文本（供命令层格式化）。
pub fn duration_text(live_start: i64) -> String {
    format_duration(live_start)
}

/// 拉取 B站直播间元数据（get_info 接口，免登录）。失败返回默认（不阻塞连接）。
fn fetch_bili_meta(room: &str) -> RoomMeta {
    let url = format!("https://api.live.bilibili.com/room/v1/Room/get_info?room_id={room}");
    let headers = vec![
        ("User-Agent".to_string(), BROWSER_UA.to_string()),
        (
            "Referer".to_string(),
            format!("https://live.bilibili.com/{room}"),
        ),
    ];
    let body = match crate::http::request_sync("GET", &url, &headers, None) {
        Ok(r) => r.body,
        Err(_) => return RoomMeta::default(),
    };
    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        return RoomMeta::default();
    };
    let d = &v["data"];
    let live_status = d["live_status"].as_i64().unwrap_or(0);
    // user_cover 常为空 → 回退 keyframe（首帧缩略图）。
    let cover = {
        let uc = d["user_cover"].as_str().unwrap_or("");
        if !uc.is_empty() {
            uc.to_string()
        } else {
            d["keyframe"].as_str().unwrap_or("").to_string()
        }
    };
    RoomMeta {
        title: d["title"].as_str().unwrap_or("").to_string(),
        // get_info 无 uname → 另取主播信息（get_anchor_in_room）。
        nickname: fetch_bili_anchor_name(room),
        cover,
        room_id: d["room_id"]
            .as_i64()
            .map(|x| x.to_string())
            .unwrap_or_default(),
        live_status,
        live_start: if live_status == 1 {
            d["live_time"].as_i64().unwrap_or(0)
        } else {
            0
        },
    }
}

/// 拉取 B站主播昵称（get_anchor_in_room 接口）。失败返回空。
fn fetch_bili_anchor_name(room: &str) -> String {
    let url = format!(
        "https://api.live.bilibili.com/live_user/v1/UserInfo/get_anchor_in_room?roomid={room}"
    );
    let headers = vec![
        ("User-Agent".to_string(), BROWSER_UA.to_string()),
        (
            "Referer".to_string(),
            format!("https://live.bilibili.com/{room}"),
        ),
    ];
    let Ok(r) = crate::http::request_sync("GET", &url, &headers, None) else {
        return String::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&r.body) else {
        return String::new();
    };
    v["data"]["info"]["uname"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// 开播时长文本（「H:MM:SS」）；start=0 返回空。
fn format_duration(start: i64) -> String {
    if start <= 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(start);
    let secs = (now - start).max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    format!("{h}:{m:02}:{s:02}")
}

/// 同时监控的直播会话上限。后台会话暂停拉流（零带宽零解码），故上限放宽。
const MAX_SESSIONS: usize = 30;

type SessionMap = std::collections::HashMap<String, Arc<LiveSession>>;
/// 会话池：key = `{platform_key}:{room}`。持锁仅做增删查，消费在锁外
/// （clone Arc<LiveSession> 后操作，避免阻塞其它会话的解码线程）。
static LIVE_SESSIONS: Mutex<Option<SessionMap>> = Mutex::new(None);

fn lock_sessions() -> std::sync::MutexGuard<'static, Option<SessionMap>> {
    LIVE_SESSIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 会话 key（平台 + 房间号；同一房间号在不同平台是不同会话）。
fn session_key(platform: Platform, room: &str) -> String {
    format!("{}:{room}", platform.key())
}

fn get_session(key: &str) -> Option<Arc<LiveSession>> {
    lock_sessions().as_ref()?.get(key).cloned()
}

fn session_count() -> usize {
    lock_sessions().as_ref().map_or(0, |m| m.len())
}

fn insert_session(key: String, s: Arc<LiveSession>) {
    lock_sessions()
        .get_or_insert_with(SessionMap::new)
        .insert(key, s);
}

fn remove_session(key: &str) -> Option<Arc<LiveSession>> {
    lock_sessions().as_mut()?.remove(key)
}

fn take_all_sessions() -> Vec<Arc<LiveSession>> {
    lock_sessions()
        .take()
        .map(|m| m.into_values().collect())
        .unwrap_or_default()
}

/// 启动直播会话（解析流地址 + 启动解码线程，阻塞至拿到首帧或超时）。
/// 多路监控：同一房间（同平台）重复调用则替换旧会话；超过 MAX_SESSIONS 报错。
pub async fn start(platform: Platform, room: String) -> Result<SessionInfo, String> {
    let key = session_key(platform, &room);
    // 同房间已存在：先停旧会话（重连语义）。
    if let Some(old) = remove_session(&key) {
        old.stop.store(true, Ordering::SeqCst);
    }
    if session_count() >= MAX_SESSIONS {
        return Err(format!("同时监控的直播间已达上限（{MAX_SESSIONS} 个）"));
    }
    // 初始校验：房间可解析、可开播（失败快速反馈给前端）；
    // 实际拉流地址由解码线程每次连接/重连时重新获取（新 expires，自愈临时故障）。
    match platform {
        Platform::Bilibili => {
            resolve_stream(&room, 10000).await?;
        }
        Platform::Douyin => {
            // 仅做一次解析校验（能否拿到流）；实际拉流由解码线程每次重连时重新解析。
            crate::douyin::resolve(&room).await?;
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(Mutex::new(Frames {
        queue: std::collections::VecDeque::new(),
        latest: None,
        scan_cache: None,
        next_seq: 0,
    }));
    let fail: FailFlag = Arc::new(Mutex::new(None));
    let paused = Arc::new(AtomicBool::new(false));
    let (room2, stop2, frames2, fail2, paused2) = (
        room.clone(),
        stop.clone(),
        frames.clone(),
        fail.clone(),
        paused.clone(),
    );
    std::thread::Builder::new()
        .name(format!("live-{room}"))
        .spawn(move || decode_loop(platform, room2, stop2, frames2, fail2, paused2))
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
                    let id = session_key(platform, &room);
                    // 元数据（B站 get_info；抖音暂空）。
                    let meta = if platform == Platform::Bilibili {
                        fetch_bili_meta(&room)
                    } else {
                        RoomMeta::default()
                    };
                    insert_session(
                        id.clone(),
                        Arc::new(LiveSession {
                            id: id.clone(),
                            stop,
                            paused,
                            frames,
                            room: room.clone(),
                            platform,
                            last_view: Mutex::new(None),
                            last_sent: Mutex::new(None),
                            meta: Mutex::new(meta.clone()),
                        }),
                    );
                    // 新会话默认置为前台（恢复拉流），暂停其余（同时只前台 1 路）。
                    set_active(&id);
                    return Ok(SessionInfo {
                        id,
                        room: room.clone(),
                        platform: platform.key().to_string(),
                        w,
                        h,
                        title: meta.title,
                        nickname: meta.nickname,
                        cover: meta.cover,
                        room_id: meta.room_id,
                        duration: format_duration(meta.live_start),
                    });
                }
            }
        }
        if stop.load(Ordering::SeqCst) {
            return Err("直播会话已被取消".to_string());
        }
        // 解码线程回报致命失败：立即返回明确错误，不必干等 15 秒超时。
        if let Some(reason) = fail.lock().unwrap_or_else(PoisonError::into_inner).take() {
            stop.store(true, Ordering::SeqCst);
            return Err(reason);
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

/// 停止指定房间的直播会话；room 为空则停止全部。
pub fn stop_room(platform: Platform, room: &str) {
    let key = session_key(platform, room);
    if let Some(s) = remove_session(&key) {
        s.stop.store(true, Ordering::SeqCst);
        log::info!("直播会话已断开（{} {}）", s.platform.label(), s.room);
    }
    // 被停的是前台会话：前台清空（无自动接管，由前端切换）。
    if active_id().as_deref() == Some(key.as_str()) {
        set_active_id(None);
    }
}

/// 停止全部直播会话。
pub fn stop_all() {
    set_active_id(None);
    for s in take_all_sessions() {
        s.stop.store(true, Ordering::SeqCst);
        log::info!("直播会话已断开（{} {}）", s.platform.label(), s.room);
    }
}

/// 活跃会话列表（前端房间列表用）。
pub fn list_sessions() -> Vec<SessionInfo> {
    let sessions = lock_sessions();
    let Some(map) = sessions.as_ref() else {
        return Vec::new();
    };
    map.values()
        .map(|s| {
            let (w, h) = {
                let f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
                f.latest.as_ref().map_or((0, 0), |b| (b.w, b.h))
            };
            let m = s
                .meta
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            SessionInfo {
                id: s.id.clone(),
                room: s.room.clone(),
                platform: s.platform.key().to_string(),
                w,
                h,
                title: m.title,
                nickname: m.nickname,
                cover: m.cover,
                room_id: m.room_id,
                duration: format_duration(m.live_start),
            }
        })
        .collect()
}

/// 是否存在任意活跃会话。
pub fn active() -> bool {
    session_count() > 0
}

/// 诊断指定会话：最新解码帧的 (seq, 媒体时间戳, 队列深度)。
pub fn diag(id: &str) -> Option<(u64, u32, usize)> {
    let s = get_session(id)?;
    let f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
    let b = f.latest.as_ref()?;
    Some((b.seq, b.ts, f.queue.len()))
}

/// 扫码取帧输出的最长边上限（像素）：超过则等比缩小以控制 YUV→RGB 成本；
/// 1080p 及以下流不受影响（原分辨率输出）。
const SCAN_MAX_LONG_SIDE: u32 = 1920;

/// 扫码取帧的输出尺寸：裁剪区不超过上限时原分辨率输出（保二维码模块细节），
/// 超过则等比缩小到最长边 = SCAN_MAX_LONG_SIDE。
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

/// 扫码消费者（多会话）：取指定会话的最新帧**当前显示视口**区域
/// （原分辨率 RGB，超上限才等比缩小）。视口未知时用全帧。
pub fn take_scan_frame(id: &str) -> Option<(Vec<u8>, u32, u32)> {
    let s = get_session(id)?;
    let view = *s.last_view.lock().unwrap_or_else(PoisonError::into_inner);
    // 只在锁内取「帧引用 + 缓存命中判断」，**不在锁内做 YUV→RGB 转换**：
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
        // 无新帧且缓存命中（视口未变）：复用上次 RGB，保证供帧率 100%。
        let cached = f
            .scan_cache
            .as_ref()
            .filter(|c| c.seq == frame.seq && c.crop == crop)
            .map(|c| (c.rgb.clone(), c.w, c.h));
        (frame, crop, cached)
    };
    if let Some((rgb, w, h)) = cached {
        return Some((rgb.as_ref().clone(), w, h));
    }
    let (fw, fh) = (frame.w, frame.h);
    // 原分辨率输出（保二维码模块细节），仅超上限才等比缩小。
    // 此前无条件减半，导致全屏视口下二维码模块明显过小、无法识别（放大才可识别）。
    let crop_rect = crop.unwrap_or((0, 0, fw, fh));
    let (ow, oh) = scan_output_size(crop_rect.2, crop_rect.3);
    let rgb = yuv_crop_to_rgb(
        &frame.y,
        &frame.u,
        &frame.v,
        fw as usize,
        fh as usize,
        crop_rect,
        ow as usize,
        oh as usize,
        1,
    );
    let rgb = Arc::new(rgb);
    // 回写缓存（短锁）。
    {
        let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
        f.scan_cache = Some(ScanCache {
            seq: frame.seq,
            crop,
            rgb: rgb.clone(),
            w: ow,
            h: oh,
        });
    }
    Some((rgb.as_ref().clone(), ow, oh))
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
    let Some(s) = get_session(id) else {
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
            return FrameOut::Empty;
        }
        *last = Some(key);
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

/// 构建房间的清晰度降级链：仅保留房间实际提供、且 ≤10000 的档位（不主动上 4K/杜比），
/// 降序排列。房间仅原画时链长为 1（不降级，避免无效重连）。
fn build_qn_chain(accept: &[u32]) -> Vec<u32> {
    let mut chain: Vec<u32> = accept.iter().copied().filter(|q| *q <= 10000).collect();
    chain.sort_unstable_by(|a, b| b.cmp(a));
    chain.dedup();
    if chain.is_empty() {
        // accept_qn 解析失败/异常：退化为「仅原画」，不降级。
        chain.push(10000);
    }
    chain
}

/// 直播追帧阈值（毫秒）：输出帧媒体时间戳落后「应有值」超过此值即追帧。
///
/// 2026-10-11（MF 硬解落地后）：解码吞吐大幅改善（实测 720p 平均单帧
/// 14.6ms、1080p 同量级，远低于典型帧间隔），追帧从「常态工作」退化为
/// 安全网。阈值由 400ms 下调到 300ms：吞吐充分时追帧仍不触发，而偶发
/// 卡顿/慢帧场景能更快把显示延迟收回来。
/// 阈值越低延迟越低、跳帧越频繁；300-600ms 是「扫码抢码」场景的合理折中。
const CATCHUP_LAG_MS: i64 = 300;

/// 解码持续失败的容忍次数：超过则降级清晰度重试。
const DECODE_FAIL_LIMIT: u32 = 40;

/// 持续追帧上限：连续快速追帧（间隔 < 4s）达到此次数，判为解码吞吐持续不足，
/// 降级清晰度换低码率流。仅「持续跟不上」才降级，偶发追帧不触发。
const CATCHUP_LIMIT: u32 = 4;

/// 「连续追帧」判定的时间窗口：两次追帧间隔超过此值即视为偶发，不累计。
const CATCHUP_WINDOW_MS: u64 = 4000;

/// 解码线程向 start() 回报的致命失败原因（未开播 / 解码不支持 / 连续重连失败）。
/// start() 轮询它以便快速返回明确错误，而非干等 15 秒首帧超时。
type FailFlag = Arc<Mutex<Option<String>>>;

/// 记录致命失败并让解码线程返回。
fn set_fail(fail: &FailFlag, reason: &str) {
    *fail.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason.to_string());
}

fn decode_loop(
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
fn remove_session_if(frames: &Arc<Mutex<Frames>>) {
    let mut sessions = lock_sessions();
    let Some(map) = sessions.as_mut() else {
        return;
    };
    let matched_key = map
        .iter()
        .find(|(_, s)| Arc::ptr_eq(&s.frames, frames))
        .map(|(k, _)| k.clone());
    if let Some(k) = matched_key {
        map.remove(&k);
        log::info!("直播会话已关闭（流结束/重连失败），回退屏幕预览与扫码");
    }
}

/// HLS 会话：拉 m3u8 -> init 段取 SPS/PPS -> 循环拉媒体段解码。
///
/// 低延迟策略：LL-HLS 1 秒分片，每轮取 seq 比上次新的分片；首次或积压时跳到
/// 最近的关键帧段（丢弃中间积压），把延迟压在 1~2 个分片内。
fn decode_hls_session(
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
    // 喂 SPS/PPS（清 VUI 兼容旧软解路径；对 MF 无害）。
    let (params, _) = strip_sps_vui(&init_info.params);
    if let Err(e) = decoder.decode(&params, 0) {
        log::warn!("HLS 参数集解码失败: {e}");
    }

    let mut last_seq = playlist.media_sequence.saturating_sub(1);
    let mut skipped_first = false;
    let mut fail_streak = 0u32;
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
                if s.offset + s.size > seg_bytes.len() {
                    continue;
                }
                let annexb = avcc_to_annexb(&seg_bytes[s.offset..s.offset + s.size]);
                if annexb.is_empty() {
                    continue;
                }
                let media_ts = (s.time * 1000 / timescale) as u32;
                decode_and_push(&mut decoder, &annexb, media_ts, frames);
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

/// 解码一个 Annex-B 访问单元并把产出的帧推入帧总线（HLS 路径用）。
/// 单帧错误静默（持续失败由上层重连兜底）。
fn decode_and_push(
    decoder: &mut dyn VideoDecoder,
    annexb: &[u8],
    media_ts: u32,
    frames: &Arc<Mutex<Frames>>,
) {
    if let Ok(list) = decoder.decode(annexb, media_ts) {
        for of in list {
            push_decoded(frames, media_ts, of);
        }
    }
}

/// 把解码产出帧推入帧总线（HLS 与 FLV 路径共用）。
fn push_decoded(frames: &Arc<Mutex<Frames>>, media_ts: u32, of: DecodedFrame) {
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

/// 慢流判定读超时：CDN 节点 3 秒不推数据 → read 超时 → 换节点。
const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// 手写 HTTP/1.1 流式读取（http-flv 用）。
///
/// 为什么不用 ureq：实测 ureq 2.12 的 `into_reader()` 对 B站 http-flv 的
/// chunked 长连接会卡死（10 秒仅读到 2 个 tag，curl 同流 60fps 正常）。
/// 这里手写 GET + chunked 解码，读超时兼作慢流检测。
pub struct HttpFlvReader {
    stream: Box<dyn Read + Send>,
    /// 响应是否用 chunked 传输编码。
    /// **关键**：B站不同 CDN 服务器行为不一——JSP3 返回 chunked，Tengine 返回
    /// 裸 FLV（Connection: close 直出）。必须按响应头区分，否则裸流会被误当
    /// chunk 头解析（"FLV\x01" 被当 hex 大小行）而读不出任何数据。
    chunked: bool,
    /// 当前 chunk 剩余字节（仅 chunked 模式使用）。
    chunk_left: usize,
    done: bool,
}

impl Read for HttpFlvReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        // 裸 FLV 模式：直接透传底层流（Connection: close 直出，读到 EOF 为止）。
        if !self.chunked {
            return self.stream.read(buf);
        }
        if self.chunk_left == 0 {
            // 读 chunk 大小行（hex + CRLF）。
            let mut size_line = Vec::with_capacity(16);
            loop {
                let mut b = [0u8; 1];
                if self.stream.read(&mut b)? == 0 {
                    return Ok(0);
                }
                size_line.push(b[0]);
                if size_line.ends_with(b"\r\n") {
                    break;
                }
                if size_line.len() > 32 {
                    return Err(std::io::Error::other("chunk 大小行过长"));
                }
            }
            let hex = String::from_utf8_lossy(&size_line[..size_line.len() - 2]);
            let size = usize::from_str_radix(hex.trim(), 16).map_err(std::io::Error::other)?;
            if size == 0 {
                // 终止 chunk：吃掉结尾 CRLF。
                let mut crlf = [0u8; 2];
                let _ = self.stream.read(&mut crlf)?;
                self.done = true;
                return Ok(0);
            }
            self.chunk_left = size;
        }
        let n = buf.len().min(self.chunk_left);
        let got = self.stream.read(&mut buf[..n])?;
        self.chunk_left -= got;
        if self.chunk_left == 0 {
            // chunk 数据后的 CRLF。
            let mut crlf = [0u8; 2];
            let _ = self.stream.read(&mut crlf)?;
        }
        Ok(got)
    }
}

impl HttpFlvReader {
    /// 连接 https://... 并发起 GET，返回已就绪的流式读取器（响应头已消费）。
    ///
    /// **跟随重定向**：B站 CDN 常返回 302 跳转到实际流节点（curl -L 同行为）。
    /// 早期实现把 302 当"节点不可用"直接失败，导致永远读不到流。
    pub fn connect(url: &str, referer: &str, cookie: Option<&str>) -> Result<Self, String> {
        let mut current = url.to_string();
        // 302 跳转链一般 1~2 跳，给足 6 次冗余。
        for _ in 0..6 {
            let (https, host, port, path) =
                crate::http::split_url(&current).map_err(|e| format!("流地址解析失败: {e}"))?;
            if !https {
                return Err("明文 http 已禁用".to_string());
            }
            // rustls 优先，握手被拒时回退 schannel：B站 CDN rustls 正常；
            // 抖音 CDN 对 rustls 指纹回 ProtocolVersion（回退 schannel 后可连）。
            // 读超时兼作慢流检测（3 秒无数据换节点）。
            let mut tls = crate::http::stream_tls_connect(&host, port, STREAM_READ_TIMEOUT)?;
            let cookie_line = cookie
                .map(|c| format!("Cookie: {c}\r\n"))
                .unwrap_or_default();
            write!(
                tls,
                "GET {path} HTTP/1.1
Host: {host}
User-Agent: {BROWSER_UA}
Referer: {referer}
{cookie_line}Accept: */*
Connection: keep-alive

"
            )
            .map_err(|e| format!("发送请求失败: {e}"))?;
            // rustls Writer 有内部缓冲：必须 flush 请求才会真正发出。
            tls.flush()
                .map_err(|e| format!("发送请求失败(flush): {e}"))?;
            // 读响应头直到空行（缓冲式读取：一次读一块，避免逐字节 syscall 与
            // 单字节读在慢首包下的脆弱性）。
            let mut headers = Vec::with_capacity(1024);
            let mut buf = [0u8; 4096];
            loop {
                let n = tls
                    .read(&mut buf)
                    .map_err(|e| format!("读取响应头失败: {e}"))?;
                if n == 0 {
                    return Err("响应中断".to_string());
                }
                headers.extend_from_slice(&buf[..n]);
                if headers.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if headers.len() > 16 * 1024 {
                    return Err("响应头过大".to_string());
                }
            }
            // 只保留响应头部分（缓冲可能多读了首块 body 的前几个字节）。
            let hdr_end = headers
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4)
                .unwrap_or(headers.len());
            let leftover: Vec<u8> = headers[hdr_end..].to_vec();
            headers.truncate(hdr_end);
            // 状态行解析：HTTP/1.x <code>。
            let status = headers
                .split(|&b| b == b' ')
                .nth(1)
                .and_then(|s| std::str::from_utf8(s).ok())
                .and_then(|s| s.parse::<u16>().ok())
                .ok_or_else(|| {
                    format!(
                        "无法解析 HTTP 状态行: {}",
                        String::from_utf8_lossy(&headers[..headers.len().min(80)])
                    )
                })?;
            // 3xx 重定向：读 Location 并跟随（B站 CDN 常态）。
            if (300..400).contains(&status) {
                let loc = headers
                    .split(|&b| b == b'\n')
                    .find_map(|line| {
                        let line = line.strip_suffix(b"\r").unwrap_or(line);
                        let (k, v) = line.split_at(line.iter().position(|&b| b == b':')?);
                        if k.eq_ignore_ascii_case(b"location") {
                            Some(v[1..].trim_ascii())
                        } else {
                            None
                        }
                    })
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .ok_or_else(|| format!("HTTP {status} 重定向缺少 Location 头"))?;
                current = if loc.starts_with("http://") || loc.starts_with("https://") {
                    loc.to_string()
                } else {
                    format!("https://{host}{loc}")
                };
                continue;
            }
            if status != 200 {
                return Err(format!(
                    "直播流 HTTP {status}: {}",
                    String::from_utf8_lossy(&headers[..headers.len().min(120)])
                ));
            }
            // 依响应头判定传输编码：有 Transfer-Encoding: chunked 才走 chunked 解析。
            let chunked = headers
                .split(|&b| b == b'\n')
                .filter_map(|line| {
                    let line = line.strip_suffix(b"\r").unwrap_or(line);
                    let (k, v) = line.split_at(line.iter().position(|&b| b == b':')?);
                    Some((k, v[1..].trim_ascii()))
                })
                .any(|(k, v)| {
                    k.eq_ignore_ascii_case(b"transfer-encoding")
                        && v.eq_ignore_ascii_case(b"chunked")
                });
            log::info!(
                "直播流已连接（传输编码: {}）",
                if chunked { "chunked" } else { "raw" }
            );
            // 响应头读取时多读的 body 前置字节拼回流**最前端**（流级别前缀），
            // 让它们照常进入后续解析——直接透传会破坏 chunk 头/FLV 边界。
            let stream = std::io::Cursor::new(leftover).chain(tls);
            return Ok(HttpFlvReader {
                stream: Box::new(stream),
                chunked,
                chunk_left: 0,
                done: false,
            });
        }
        Err("重定向次数过多（>6）".to_string())
    }
}

/// 从流中精确读取 n 字节（FLV tag 头/体用）；EOF/超时返回「流已结束」。
fn read_exact(reader: &mut impl Read, buf: &mut Vec<u8>, n: usize) -> Result<(), String> {
    buf.resize(n, 0);
    reader
        .read_exact(&mut buf[..n])
        .map_err(|_| "流已结束".to_string())
}

fn decode_session(
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
    // 恢复同步：从暂停恢复后，丢弃帧直到「恢复后经过一段墙钟」且到达关键帧，
    // 直接跳到最新画面（避免重放暂停期间积压的旧帧）。
    // was_paused 确保仅「暂停→恢复」触发，全新会话首帧不延迟。
    let mut resync_until: Option<std::time::Instant> = None;
    let mut was_paused = false;
    // 暂停开始时刻（用于按暂停时长推算需要丢弃的积压窗口）。
    let mut pause_started: Option<std::time::Instant> = None;
    // 连续快速追帧计数（间隔 < CATCHUP_WINDOW_MS 才累加；偶发追帧不复位也不累加）。
    // 达 CATCHUP_LIMIT 判为解码吞吐持续不足 → 降级清晰度（新会话计数从 0 起）。
    let mut catchup_count = 0u32;
    let mut last_catchup_at: Option<std::time::Instant> = None;
    // 缓存的 SPS/PPS（追帧重建解码器后需重喂）。
    let mut param_sets: Vec<u8> = Vec::new();
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
            resync_until = None;
            was_paused = true;
            if pause_started.is_none() {
                pause_started = Some(std::time::Instant::now());
            }
            last_data_at = std::time::Instant::now();
            window_start = std::time::Instant::now();
            window_frames = 0;
            std::thread::sleep(Duration::from_millis(80));
            continue;
        }
        // 从暂停恢复：按暂停时长推算积压窗口（封顶 3s），丢弃该窗口内的旧帧，
        // 排空 socket/TCP 积压 → 直接跳到最新画面。
        if was_paused && resync_until.is_none() {
            let paused_ms = pause_started
                .take()
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(800)
                .clamp(400, 3000);
            resync_until = Some(std::time::Instant::now() + Duration::from_millis(paused_ms));
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
        let body = &tag[..data_size];
        let first = body[0];
        let frame_type = first >> 4;
        if first & 0x0F != 7 {
            continue; // 非 AVC
        }

        // ---- 恢复同步：丢弃「丢弃窗口」内的所有帧，仅在窗口结束后的首个关键帧恢复 ----
        // 动机：暂停恢复后 socket/TCP 可能残留积压旧帧，逐帧播出表现为「慢速快进」。
        // 做法：丢弃窗口（800ms）内所有帧；窗口结束且遇到关键帧时，从该帧开始解码
        // （直接跳到最新画面）。
        if let Some(deadline) = resync_until {
            let now = std::time::Instant::now();
            if now < deadline || frame_type != 1 {
                continue; // 仍在丢弃窗口内，或尚未到关键帧
            }
            // 窗口结束 + 关键帧：恢复正常解码。
            resync_until = None;
            anchor = Some((media_ts, now));
        }

        // ---- 直播追帧（安全网）----
        // 落后量 = 应有媒体时间戳（锚点 + 已过墙钟）− 当前 tag 媒体时间戳。
        // 解码吞吐不足时 socket 缓冲持续积压、落后量单调增长 → 触发追帧；
        // 追帧后锚点重置。asm 生效后正常流落后量恒 0（实测 35 秒零触发）。
        let now = std::time::Instant::now();
        match anchor {
            None => anchor = Some((media_ts, now)),
            Some((a_ts, a_at)) => {
                let expected = a_ts as i64 + a_at.elapsed().as_millis() as i64;
                let lag = expected - media_ts as i64;
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

        // 非参考帧（frame_type=3，disposable inter）不影响后续解码的参考链，
        // 直接跳过解码：若 60fps 流用它做时间分层，可近乎减半解码负载（1080p60
        // 单线程跟不上时尤为关键）。扫码/预览少这些帧无碍。
        if frame_type == 3 && body[1] == 1 {
            continue;
        }

        match body[1] {
            0 => {
                // AVC sequence header：AVCDecoderConfigurationRecord → SPS/PPS。
                param_sets = avcc_config_to_annexb(&body[5..]);
                log::info!(
                    "AVC 序列参数集: {} 字节（体前 8 字节 {:02x?}）",
                    param_sets.len(),
                    &body[..8]
                );
                if !param_sets.is_empty() {
                    log::info!(
                        "参数集前 16 字节: {:02x?}",
                        &param_sets[..param_sets.len().min(16)]
                    );
                    // 清除带 VUI 的 SPS（历史兼容保险，见 strip_sps_vui 文档；
                    // VUI 对解码/扫码无用，仅翻转比特）。
                    let (stripped, did_strip) = strip_sps_vui(&param_sets);
                    let to_decode = if did_strip {
                        log::info!("已清除 SPS VUI（历史兼容保险）");
                        &stripped
                    } else {
                        &param_sets
                    };
                    if let Err(e) = decoder.decode(to_decode, media_ts) {
                        log::warn!("参数集解码失败: {e}");
                    } else if did_strip {
                        param_sets = stripped;
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
                // IDR 帧常内嵌一份带 VUI 的重复 SPS（历史兼容保险，见 strip_sps_vui），
                // 解码前清除。非 IDR 帧不含 SPS，跳过以免无谓遍历。
                let annexb = if frame_type == 1 {
                    let (s, changed) = strip_sps_vui(&annexb);
                    if changed {
                        s
                    } else {
                        annexb
                    }
                } else {
                    annexb
                };
                // 组合时间（body[2..5]）忽略——显示延迟可接受。
                // 解码错误不静默：持续失败（编码不兼容等）由上层降级清晰度。
                let decode_t = std::time::Instant::now();
                match decoder.decode(&annexb, media_ts) {
                    Ok(list) => {
                        decode_ns_total += decode_t.elapsed().as_nanos();
                        dec_fails = 0; // 成功即复位：dec_fails 是「连续失败」计数
                        if list.is_empty() {
                            out_none += 1;
                        } else {
                            out_ok += 1;
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
                    log::info!(
                        "解码心跳: 出帧 {out_ok} / 无输出 {out_none} / 错误 {out_err}，队列 {qlen}，输入滞后 {in_lag}ms，输出滞后 {out_lag}ms，平均单帧解码 {avg_ms:.2}ms"
                    );
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
fn wait_with_first_frame(frames: &Arc<Mutex<Frames>>, secs: u64) -> u64 {
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
