use serde_json::Value;

use crate::http::header_utf8;

pub(super) const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

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
pub(super) type BiliStreamResult = (Vec<(String, String)>, Vec<u32>);

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
pub(super) async fn resolve_stream(room: &str, qn: u32) -> Result<(String, String), String> {
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
                    crate::http::excerpt(&resp.body, 120)
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
                    crate::http::excerpt(&resp2.body, 120)
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
pub(super) fn host_of(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

/// 按 host 去重（保留首次出现顺序），避免同一节点重复出现在候选列表。
pub(super) fn dedup_by_host(urls: Vec<String>) -> Vec<String> {
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

/// 构建房间的清晰度降级链：仅保留房间实际提供、且 ≤10000 的档位（不主动上 4K/杜比），
/// 降序排列。房间仅原画时链长为 1（不降级，避免无效重连）。
pub(super) fn build_qn_chain(accept: &[u32]) -> Vec<u32> {
    let mut chain: Vec<u32> = accept.iter().copied().filter(|q| *q <= 10000).collect();
    chain.sort_unstable_by(|a, b| b.cmp(a));
    chain.dedup();
    if chain.is_empty() {
        // accept_qn 解析失败/异常：退化为「仅原画」，不降级。
        chain.push(10000);
    }
    chain
}
