//! 抖音直播间流地址解析（方案 A：ttwid-only，三步纯 HTTP，无签名）。
//!
//! 路径（2026-09 实测仅凭 ttwid 可用）：
//! 1. 注册 ttwid（Bytedance Web API 通行凭证，无需浏览器）；
//! 2. `GET /webcast/room/web/enter/?web_rid=<rid>`（带 ttwid）取 room 数据；
//! 3. 从 `flv_pull_url`（或 `live_core_sdk_data.pull_data.stream_data`）取 http-flv 直链，
//!    明文 http 统一升级为 https（本工具禁用明文 HTTP）。
//!
//! 直链约 5~30 分钟过期、a_bogus 签名约 2~3 周变动：前者由 decode_loop 的
//! 「断流 → 重新解析 → 重连」机制自愈；后者属方案 B（加固）范畴，首发不引入。
//!
//! 抖音端到端延迟 ~1-2s 为协议层差异（仅 HTTP-FLV/HLS，低延迟 RTM 私有不可得），
//! 无法通过实现优化消除。

use serde_json::Value;

use crate::http::{header_utf8, Headers};

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// enter 接口 URL（取 room 数据与流地址）。web_rid = 直播间号。
/// 参数对齐抖音 web 端（含 app_name/live_id/screen 尺寸等，降低风控概率）。
fn enter_url(web_rid: &str) -> String {
    format!(
        "https://live.douyin.com/webcast/room/web/enter/?aid=6383&app_name=douyin_web&live_id=1&device_platform=web&language=zh-CN&cookie_enabled=true&screen_width=1920&screen_height=1080&browser_language=zh-CN&browser_platform=Win32&browser_name=Chrome&browser_version=126.0.0.0&web_rid={web_rid}&enter_from=web_live&is_need_double_stream=false"
    )
}

/// 解析抖音直播间为 http-flv 直链，返回 (url, referer, cookie)。
pub async fn resolve(web_rid: &str) -> Result<(String, String, String), String> {
    let ttwid = fetch_ttwid().await?;
    let (url, referer) = enter(web_rid, &ttwid).await?;
    Ok((url, referer, format!("ttwid={ttwid}")))
}

/// 同步版（解码线程重连时调用；抖音直链过期需重新解析以取全新地址）。
pub fn resolve_sync(web_rid: &str) -> Result<(String, String, String), String> {
    let ttwid = fetch_ttwid_sync()?;
    let (url, referer) = enter_sync(web_rid, &ttwid)?;
    Ok((url, referer, format!("ttwid={ttwid}")))
}

/// 从响应 Set-Cookie 提取 ttwid 值。
fn extract_ttwid(resp: &crate::http::Response) -> Result<String, String> {
    for s in resp.headers_all("set-cookie") {
        if let Some(rest) = s.trim().strip_prefix("ttwid=") {
            let val = rest.split(';').next().unwrap_or("").trim();
            if !val.is_empty() {
                return Ok(val.to_string());
            }
        }
    }
    Err("抖音 ttwid 获取失败（响应无 Set-Cookie）".to_string())
}

/// 访问抖音直播首页以获取 ttwid Cookie（Web API 通行凭证，无需浏览器）。
/// 注：旧的 ttwid.bytedance.com 注册端点已失效（404），改从首页 Set-Cookie 取。
async fn fetch_ttwid() -> Result<String, String> {
    let resp = crate::http::get(
        "https://live.douyin.com/",
        vec![("User-Agent".to_string(), header_utf8(BROWSER_UA))],
    )
    .await
    .map_err(|e| format!("抖音 ttwid 获取失败: {e}"))?;
    extract_ttwid(&resp)
}

/// ttwid 获取（sync，解码线程重连用）。
fn fetch_ttwid_sync() -> Result<String, String> {
    let resp = crate::http::request_sync(
        "GET",
        "https://live.douyin.com/",
        &vec![("User-Agent".to_string(), header_utf8(BROWSER_UA))],
        None,
    )
    .map_err(|e| format!("抖音 ttwid 获取失败: {e}"))?;
    extract_ttwid(&resp)
}

fn enter_headers(web_rid: &str, ttwid: &str) -> (Headers, String) {
    let referer = format!("https://live.douyin.com/{web_rid}");
    let headers = vec![
        ("User-Agent".to_string(), header_utf8(BROWSER_UA)),
        ("Referer".to_string(), referer.clone()),
        ("Cookie".to_string(), format!("ttwid={ttwid}")),
    ];
    (headers, referer)
}

/// 请求 enter 接口并解析流地址（async）。
async fn enter(web_rid: &str, ttwid: &str) -> Result<(String, String), String> {
    let (headers, referer) = enter_headers(web_rid, ttwid);
    let resp = crate::http::get(&enter_url(web_rid), headers)
        .await
        .map_err(|e| format!("抖音直播接口请求失败: {e}"))?;
    parse_enter_body(&resp.body, web_rid, &referer)
}

/// 请求 enter 接口并解析流地址（sync）。
fn enter_sync(web_rid: &str, ttwid: &str) -> Result<(String, String), String> {
    let (headers, referer) = enter_headers(web_rid, ttwid);
    let resp = crate::http::request_sync("GET", &enter_url(web_rid), &headers, None)
        .map_err(|e| format!("抖音直播接口请求失败: {e}"))?;
    parse_enter_body(&resp.body, web_rid, &referer)
}

fn parse_enter_body(body: &str, web_rid: &str, referer: &str) -> Result<(String, String), String> {
    let v: Value = serde_json::from_str(body)
        .map_err(|_| format!("抖音直播接口响应非 JSON: {}", &body[..body.len().min(120)]))?;
    parse_douyin_flv(&v)
        .map(|u| (u, referer.to_string()))
        .ok_or_else(|| {
            format!("抖音直播间 {web_rid} 未找到可用的 FLV 流（可能未开播、房间号有误或接口风控）")
        })
}

/// 明文 http 升级为 https（HttpFlvReader 禁用明文 HTTP）。
fn https_ify(u: &str) -> String {
    match u.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => u.to_string(),
    }
}

/// 抖音直播间元数据。
#[derive(Default, Clone)]
pub struct DouyinMeta {
    pub title: String,
    pub nickname: String,
    pub cover: String,
    pub room_id: String,
    /// 1 = 直播中，0 = 未开播。
    pub live_status: i64,
    pub live_start: i64,
}

/// 获取抖音直播间元数据（复用 ttwid + enter 响应解析）。
pub fn fetch_meta_sync(web_rid: &str) -> DouyinMeta {
    let ttwid = match fetch_ttwid_sync() {
        Ok(t) => t,
        Err(_) => return DouyinMeta::default(),
    };
    let (headers, _referer) = enter_headers(web_rid, &ttwid);
    let resp = match crate::http::request_sync("GET", &enter_url(web_rid), &headers, None) {
        Ok(r) => r,
        Err(_) => return DouyinMeta::default(),
    };
    let Ok(v) = serde_json::from_str::<Value>(&resp.body) else {
        return DouyinMeta::default();
    };
    parse_douyin_meta(&v)
}

/// 从 enter 响应解析元数据。
/// 结构：data.data[0].{title, cover.url_list[0], owner.nickname, id_str, status, create_time}。
pub fn parse_douyin_meta(v: &Value) -> DouyinMeta {
    let Some(d0) = v
        .get("data")
        .and_then(|d| d.get("data"))
        .and_then(|a| a.get(0))
    else {
        return DouyinMeta::default();
    };
    let title = d0["title"].as_str().unwrap_or("").to_string();
    let nickname = d0["owner"]["nickname"].as_str().unwrap_or("").to_string();
    // 封面：room.cover.url_list[0]，或 owner.avatar_thumb.url_list[0]。
    let cover = d0["cover"]["url_list"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .or_else(|| {
            d0["owner"]["avatar_thumb"]["url_list"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let room_id = d0["id_str"].as_str().unwrap_or("").to_string();
    // status: 2 = 直播中（抖音约定）。
    let status = d0["status"].as_i64().unwrap_or(0);
    let live_status = if status == 2 { 1 } else { 0 };
    let live_start = if live_status == 1 {
        d0["create_time"].as_i64().unwrap_or(0)
    } else {
        0
    };
    DouyinMeta {
        title,
        nickname,
        cover,
        room_id,
        live_status,
        live_start,
    }
}

/// 从 enter 响应提取 flv 拉流地址（多代接口结构兼容，优先最高清晰度）。
pub fn parse_douyin_flv(v: &Value) -> Option<String> {
    let d0 = v.get("data")?.get("data")?.get(0)?;
    // 路径一：stream_url.flv_pull_url（清晰度名 → url）。
    if let Some(obj) = d0["stream_url"]["flv_pull_url"].as_object() {
        for key in ["full_hd1", "hd1", "sd1", "sd2"] {
            if let Some(u) = obj.get(key).and_then(|x| x.as_str()) {
                if !u.is_empty() {
                    return Some(https_ify(u));
                }
            }
        }
        if let Some(u) = obj
            .iter()
            .find(|(_, val)| val.as_str().is_some_and(|s| !s.is_empty()))
            .and_then(|(_, val)| val.as_str())
        {
            return Some(https_ify(u));
        }
    }
    // 路径二：live_core_sdk_data.pull_data.stream_data（JSON 字符串）→ data.*.main.flv。
    if let Some(s) = d0["stream_url"]["live_core_sdk_data"]["pull_data"]["stream_data"].as_str() {
        if let Ok(sd) = serde_json::from_str::<Value>(s) {
            if let Some(routes) = sd.get("data").and_then(|d| d.as_object()) {
                for route in routes.values() {
                    if let Some(u) = route["main"]["flv"].as_str() {
                        if !u.is_empty() {
                            return Some(https_ify(u));
                        }
                    }
                }
            }
        }
    }
    None
}
