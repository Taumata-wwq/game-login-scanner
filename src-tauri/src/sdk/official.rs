//! 官服 SDK：App 扫码登录（createQRLogin + queryQRLoginStatus，米游社 App 扫码确认）
//! + combo/panda 扫码（scan/confirm，直接用 stoken 确认）。

use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};

use crate::constants;
use crate::crypto;

use crate::http::Headers;

fn insert_header(h: &mut Headers, k: &str, v: &str) {
    h.push((k.to_string(), crate::http::header_utf8(v)));
}

fn now_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// DS2 签名纯函数：`{t},{r},{md5("salt={salt}&t={t}&r={r}&b={body}&q=")}`。
pub fn ds_payload(salt: &str, t: &str, r: &str, body: &str) -> String {
    let m = format!("salt={salt}&t={t}&r={r}&b={body}&q=");
    let md5 = crypto::md5_hex(&m);
    format!("{t},{r},{md5}")
}

/// DS 签名生成。盐为空时返回空 → 调用方不携带 DS 头（降级路径）。
fn format_ds(body: &str) -> String {
    let salt = constants::OFFICIAL_DS_SALT;
    if salt.is_empty() {
        return String::new();
    }
    let t = now_sec().to_string();
    let r: u32 = rand::thread_rng().gen_range(100001..=200000);
    ds_payload(salt, &t, &r.to_string(), body)
}

/// 生成官服 DSPROD 系列请求头（passport 接口使用）。
fn dsprod_headers(body: &str, device_id: &str, device_fp: &str) -> Headers {
    let mut h = Headers::new();
    insert_header(&mut h, "x-rpc-app_id", constants::DSPROD_APP_ID);
    insert_header(&mut h, "x-rpc-client_type", constants::DSPROD_CLIENT_TYPE);
    insert_header(&mut h, "x-rpc-device_id", device_id);
    insert_header(&mut h, "x-rpc-device_fp", device_fp);
    insert_header(&mut h, "x-rpc-device_name", "游戏扫码器-PC");
    insert_header(&mut h, "x-rpc-device_model", "Windows");
    insert_header(&mut h, "x-rpc-sys_version", "Windows 10");
    insert_header(&mut h, "x-rpc-game_biz", constants::DSPROD_GAME_BIZ);
    insert_header(&mut h, "x-rpc-app_version", constants::DSPROD_APP_VERSION);
    insert_header(&mut h, "x-rpc-sdk_version", constants::DSPROD_SDK_VERSION);
    insert_header(
        &mut h,
        "x-rpc-account_version",
        constants::DSPROD_ACCOUNT_VERSION,
    );
    let ds = format_ds(body);
    if !ds.is_empty() {
        insert_header(&mut h, "DS", &ds);
    }
    h
}

async fn post_json(
    url: &str,
    body: &str,
    mut headers: Headers,
) -> Result<(Value, Headers), String> {
    headers.push(("Content-Type".to_string(), "application/json".to_string()));
    let resp = crate::http::post(url, headers, body)
        .await
        .map_err(|e| format!("官服请求失败: {e}"))?;
    let json = serde_json::from_str(&resp.body).map_err(|_| {
        format!(
            "官服响应非 JSON: {}",
            &resp.body[..resp.body.len().min(200)]
        )
    })?;
    Ok((json, resp.headers))
}

fn retcode_err(v: &Value, fallback: &str) -> String {
    v.get("message")
        .and_then(|m| m.as_str())
        .unwrap_or(fallback)
        .to_string()
}

// ---------- 扫码登录 ----------

/// 扫码登录会话：轮询凭据 + 二维码内容。
#[derive(Debug, Clone, Serialize)]
pub struct OfficialQrSession {
    pub ticket: String,
    pub qr_url: String,
}

/// 轮询结果。
#[derive(Debug, Clone, Serialize)]
pub enum OfficialQrStatus {
    /// 等待扫码 / 等待确认
    Waiting,
    /// 已扫码，待手机确认
    Scanned,
    /// 已确认：stoken（token_type=1）+ uid/mid + 昵称
    Confirmed {
        uid: String,
        stoken: String,
        mid: String,
        uname: String,
    },
    /// 二维码已过期，需重新生成
    Expired,
}

/// 申请扫码登录二维码（米游社 App 扫码确认）。
pub async fn qr_login_start(device_id: &str, device_fp: &str) -> Result<OfficialQrSession, String> {
    let headers = dsprod_headers("{}", device_id, device_fp);
    let (json, resp_headers) = post_json(constants::OFFICIAL_QR_CREATE_URL, "{}", headers).await?;

    // 极验：先判响应头 x-rpc-aigis，再判 retcode。
    if let Some(v) = resp_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-rpc-aigis"))
        .map(|(_, v)| v.as_str())
    {
        if !v.is_empty() && v != "null" {
            return Err("触发风控验证，请稍后重试".to_string());
        }
    }
    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        return Err(retcode_err(&json, "申请二维码失败"));
    }

    let ticket = json["data"]["ticket"]
        .as_str()
        .ok_or_else(|| "响应缺少 ticket".to_string())?
        .to_string();
    let qr_url = json["data"]["url"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("https://user.mihoyo.com/qr-login.html?ticket={ticket}"));
    Ok(OfficialQrSession { ticket, qr_url })
}

/// 轮询扫码状态。Confirmed 时从 tokens 中取 token_type=1（stoken）。
pub async fn qr_login_poll(
    ticket: &str,
    device_id: &str,
    device_fp: &str,
) -> Result<OfficialQrStatus, String> {
    let body = json!({ "ticket": ticket }).to_string();
    let headers = dsprod_headers(&body, device_id, device_fp);
    let (json, _) = post_json(constants::OFFICIAL_QR_POLL_URL, &body, headers).await?;

    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = retcode_err(&json, "查询扫码状态失败");
        // 过期/作废类错误统一视为 Expired，让前端引导刷新。
        if msg.contains("过期") || msg.contains("失效") || msg.contains("expire") {
            return Ok(OfficialQrStatus::Expired);
        }
        return Err(msg);
    }

    let status = json["data"]["status"].as_str().unwrap_or("Created");
    match status {
        "Confirmed" => {
            let user_info = &json["data"]["user_info"];
            let uid = user_info["aid"]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| user_info["aid"].as_i64().map(|n| n.to_string()))
                .unwrap_or_default();
            let mid = user_info["mid"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| ticket.to_string());

            // tokens: [{token_type: 1 → stoken, 2 → ltoken}]
            let mut stoken = String::new();
            if let Some(tokens) = json["data"]["tokens"].as_array() {
                for t in tokens {
                    if t.get("token_type").and_then(|v| v.as_i64()) == Some(1) {
                        if let Some(tok) = t.get("token").and_then(|v| v.as_str()) {
                            stoken = tok.to_string();
                            break;
                        }
                    }
                }
            }
            if stoken.is_empty() {
                return Err("登录响应缺少 stoken（token_type=1），无法用于扫码确认".to_string());
            }
            let uname = user_info["nickname"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            Ok(OfficialQrStatus::Confirmed {
                uid,
                stoken,
                mid,
                uname,
            })
        }
        "Scanned" => Ok(OfficialQrStatus::Scanned),
        _ => Ok(OfficialQrStatus::Waiting),
    }
}

// ---------- combo/panda 扫码 ----------

/// 获取官服昵称（米游社 getUserFullInfo）。
/// 扫码登录响应的 user_info 只有 aid/mid（无 nickname），故单独查询；
/// 失败不影响登录，仅用于展示（返回 Err 时调用方回退 uid 显示）。
pub async fn get_uname(uid: &str, stoken: &str, mid: &str) -> Result<String, String> {
    let url = format!("https://bbs-api.miyoushe.com/user/wapi/getUserFullInfo?uid={uid}&gids=2");
    let headers: Headers = vec![
        (
            "Referer".to_string(),
            format!("https://www.miyoushe.com/ys/accountCenter/postList?id={uid}"),
        ),
        ("Cookie".to_string(), format!("stoken={stoken};mid={mid}")),
    ];
    let resp = crate::http::get(&url, headers)
        .await
        .map_err(|e| format!("获取昵称请求失败: {e}"))?;
    let json: Value = serde_json::from_str(&resp.body).map_err(|_| {
        format!(
            "昵称响应非 JSON: {}",
            &resp.body[..resp.body.len().min(200)]
        )
    })?;
    json["data"]["user_info"]["nickname"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "响应缺少 nickname".to_string())
}

/// 官服通行证扫码端点使用的完整 DSPROD 请求头。
///
/// 米哈游新版安全机制要求携带 `x-rpc-device_id`（设备绑定），否则服务端虽返回
/// retcode=0，但游戏端在校验令牌时会判定「登录状态失效」（Snap.Hutao #1345 确认）。
/// 头不完整还会让 getGameToken 返回误导性的「decode err」。
fn official_full_headers(device_id: &str) -> Headers {
    let mut h = Headers::new();
    insert_header(&mut h, "x-rpc-app_id", constants::DSPROD_APP_ID);
    insert_header(&mut h, "x-rpc-client_type", constants::DSPROD_CLIENT_TYPE);
    insert_header(&mut h, "x-rpc-device_id", device_id);
    insert_header(
        &mut h,
        "x-rpc-device_fp",
        constants::DSPROD_DEVICE_FP_DEFAULT,
    );
    insert_header(&mut h, "x-rpc-device_name", "游戏扫码器-PC");
    insert_header(&mut h, "x-rpc-device_model", "Windows");
    insert_header(&mut h, "x-rpc-sys_version", "Windows 10");
    insert_header(&mut h, "x-rpc-game_biz", constants::DSPROD_GAME_BIZ);
    insert_header(&mut h, "x-rpc-app_version", constants::DSPROD_APP_VERSION);
    insert_header(&mut h, "x-rpc-sdk_version", constants::DSPROD_SDK_VERSION);
    insert_header(
        &mut h,
        "x-rpc-account_version",
        constants::DSPROD_ACCOUNT_VERSION,
    );
    insert_header(
        &mut h,
        "User-Agent",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) miHoYoBBS/2.76.1",
    );
    insert_header(&mut h, "X-Requested-With", "com.mihoyo.hyperion");
    insert_header(&mut h, "Origin", "https://user.mihoyo.com");
    insert_header(&mut h, "Referer", "https://user.mihoyo.com/");
    h
}

/// 官服 v2 扫码确认（两阶段）：
/// 1) `combo/panda/qrcode/scan`（带 `passport_app_id`，不签名）→ 响应含 `passport_qr_url`；
/// 2) 从 `passport_qr_url` 解析 `tk`，用通行证 App 端点 `scanQRLogin` + `confirmQRLogin`
///    （Cookie `stoken=..;mid=..`）完成账号绑定。
///
/// 参考 bh3_login_simulation `QRScanner`：`handler` 收到 scan 响应后，若含
/// `passport_qr_url` 则 `parseUrl` 解析出 `tk`，再走 `newOfficialQRRunnable`/
/// `runnable2`（scanQRLogin/confirmQRLogin）。直接用游戏码 ticket 调通行证端点
/// 会被服务端接受（retcode=0）但游戏端判定「登录状态失效」——必须换用 tk。
pub async fn scan(
    ticket: &str,
    stoken: &str,
    mid: &str,
    device_id: &str,
    biz_key: &str,
    app_id: i64,
) -> Result<(), String> {
    // ---------- 阶段一：combo/panda scan（官服 v2 带 passport_app_id，不签名）----------
    let scan_body = json!({
        "app_id": app_id.to_string(),
        "device": device_id,
        "ticket": ticket,
        "ts": now_ms(),
        "passport_app_id": constants::DSPROD_APP_ID,
    })
    .to_string();
    let (json, _) = post_json(&constants::panda_scan_url(biz_key), &scan_body, Vec::new()).await?;
    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        return Err(retcode_err(&json, "官服扫码失败"));
    }

    // ---------- 阶段二：解析 passport_qr_url 的 tk，走通行证 App 端点 ----------
    let pq = json["data"]["passport_qr_url"].as_str().unwrap_or("");
    let tk = extract_tk(pq)
        .ok_or_else(|| "响应缺少 passport_qr_url（无法完成官服通行证确认）".to_string())?;

    let body = json!({ "ticket": tk, "token_types": ["1"] }).to_string();
    let mut headers = official_full_headers(device_id);
    insert_header(
        &mut headers,
        "Cookie",
        &format!("stoken={stoken};mid={mid}"),
    );

    let (j, _) = post_json(constants::OFFICIAL_SCAN_QR_URL, &body, headers.clone()).await?;
    if j.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        return Err(retcode_err(&j, "官服扫码失败"));
    }
    let (j, _) = post_json(constants::OFFICIAL_CONFIRM_QR_URL, &body, headers).await?;
    if j.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        return Err(retcode_err(&j, "官服扫码确认失败"));
    }
    Ok(())
}

/// 从通行证二维码 URL（`...mobile.html?...&tk=xxx`）提取 `tk`。
fn extract_tk(url: &str) -> Option<String> {
    for part in url.split(['?', '&']) {
        if let Some(v) = part.strip_prefix("tk=") {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}
