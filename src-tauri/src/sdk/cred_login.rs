//! 渠道账号登录（米哈游通行证 / B站）：短信验证码 + 密码。
//!
//! 极验（geetest）：米哈游/B站的验证码/密码登录可能触发人机验证——响应头返回
//! `x-rpc-aigis`（米哈游）或 code=...（B站）。此时返回 `NeedCaptcha`，前端渲染
//! 极验 Web SDK，用户完成后回传 aigis 参数重新调用。

use serde_json::{json, Value};

use crate::constants;
use crate::crypto;

/// 登录结果。
pub enum CredOutcome {
    /// 米哈游官服登录成功（stoken + mid + uid）。
    MiyousheOk {
        uid: String,
        stoken: String,
        mid: String,
    },
    /// B站登录成功（uid + access_key）。
    BiliOk { uid: String, access_key: String },
    /// 需要人机验证：返回前端渲染所需数据（session_id + gt/challenge 等）。
    NeedCaptcha(Value),
}

fn now_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 米哈游 DSPROD 头（passport 登录接口用）。
/// 必须与 official.rs 的 dsprod_headers 完全一致（含 device_name/model/sys_version），
/// 否则服务端判定「请求频繁」(-1001) 风控。
fn dsprod_headers(device_id: &str, device_fp: &str) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut h = HeaderMap::new();
    let mut set = |k: &'static str, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    };
    set("x-rpc-app_id", constants::DSPROD_APP_ID);
    set("x-rpc-client_type", constants::DSPROD_CLIENT_TYPE);
    set("x-rpc-device_id", device_id);
    set("x-rpc-device_fp", device_fp);
    set("x-rpc-device_name", "崩坏3扫码器-PC");
    set("x-rpc-device_model", "Windows");
    set("x-rpc-sys_version", "Windows 10");
    set("x-rpc-game_biz", constants::DSPROD_GAME_BIZ);
    set("x-rpc-app_version", constants::DSPROD_APP_VERSION);
    set("x-rpc-sdk_version", constants::DSPROD_SDK_VERSION);
    set("x-rpc-account_version", constants::DSPROD_ACCOUNT_VERSION);
    h
}

/// 米哈游通行证 Web 协议头（createLoginCaptcha / web login 用）。
/// 与游戏 App 协议（dsprod_headers）不同：通行证的 app_id/client_type/game_biz 都不同，
/// 极验 aigis 上下文与 app_id 绑定，用错会导致「图片验证码失效」(-302)。
fn passport_web_headers(device_id: &str, device_fp: &str) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut h = HeaderMap::new();
    let mut set = |k: &'static str, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    };
    set("x-rpc-app_id", constants::PASSPORT_WEB_APP_ID);
    set("x-rpc-client_type", "4");
    set("x-rpc-game_biz", "plat_cn");
    set("x-rpc-device_id", device_id);
    set("x-rpc-device_fp", device_fp);
    set("x-rpc-device_name", "Chrome");
    set("x-rpc-device_model", "Chrome%20152.0.0.0");
    set("x-rpc-device_os", "Windows%2010%2064-bit");
    set("x-rpc-sdk_version", "2.57.0");
    // 关键：告知服务器请求来自通行证登录页。缺失时服务器返回「游戏 App 环境」的
    // 极验配置（不同的 gt），导致极验结果与 app_id 上下文不匹配 → 图片验证码失效(-302)。
    set(
        "x-rpc-mi_referrer",
        "https://user.mihoyo.com/login-platform/index.html?app_id=dw9y09jqjpxc&theme=passport&token_type=4&game_biz=plat_cn&message_origin=https%253A%252F%252Fuser.mihoyo.com&succ_back_type=message%253Alogin-platform%253Alogin-success&fail_back_type=message%253Alogin-platform%253Alogin-fail&ux_mode=popup&iframe_level=1#/login/captcha",
    );
    set("x-rpc-lifecycle_id", &hex16());
    h
}

/// 生成 16 位十六进制随机串（用于 x-rpc-lifecycle_id）。
fn hex16() -> String {
    use rand::Rng;
    let n: u64 = rand::thread_rng().gen();
    format!("{n:016x}")
}

/// 解析米哈游登录响应：成功提取 stoken/mid/uid；触发极验则返回 NeedCaptcha。
fn parse_mihoyo_login(json: &Value, headers: &reqwest::header::HeaderMap) -> Result<CredOutcome, String> {
    // 极验：响应头 x-rpc-aigis。
    if let Some(aigis) = headers.get("x-rpc-aigis") {
        if let Ok(v) = aigis.to_str() {
            if !v.is_empty() && v != "null" {
                if let Ok(parsed) = serde_json::from_str::<Value>(v) {
                    return Ok(CredOutcome::NeedCaptcha(parsed));
                }
            }
        }
    }
    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("登录失败");
        return Err(msg.to_string());
    }
    let data = &json["data"];
    let token = data["token"]["token"].as_str().unwrap_or_default().to_string();
    let mid = data["user_info"]["mid"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_default();
    let uid = data["user_info"]["aid"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| data["user_info"]["aid"].as_i64().map(|n| n.to_string()))
        .unwrap_or_default();
    if token.is_empty() {
        return Err("登录响应缺少 token".to_string());
    }
    Ok(CredOutcome::MiyousheOk {
        uid,
        stoken: token,
        mid,
    })
}

/// 米哈游设备指纹接口。
const DEVICE_FP_URL: &str = "https://public-data-api.mihoyo.com/device-fp/api/getFp";

/// 获取真实设备指纹（风控必需）。
/// 用 device_id + 随机 seed 请求 getFp，成功则返回 device_fp（32 位 hex）。
/// 需在登录前调用并持久化，否则 x-rpc-device_fp 用假值会触发「请求频繁」(-1001)。
pub async fn fetch_device_fp(device_id: &str, seed_id: &str, seed_time: &str) -> Result<String, String> {
    let body = json!({
        "device_id": device_id,
        "seed_id": seed_id,
        "seed_time": seed_time,
        "platform": "2",
        "device_fp": "38d7ed153506e",  // 初始占位（参考项目实测可用）
        "app_name": "bh3_cn",
        "ext_fields": "{}",
    })
    .to_string();
    let resp = crate::http::client()
        .post(DEVICE_FP_URL)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("getFp 请求失败: {e}"))?;
    let json: Value = resp.json().await.map_err(|e| format!("getFp 响应解析失败: {e}"))?;
    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("未知错误");
        return Err(format!("getFp 失败: {msg}"));
    }
    json["data"]["device_fp"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "getFp 响应缺少 device_fp".to_string())
}

/// 从响应头 Set-Cookie 中解析指定 Cookie 字段的值。
fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    for v in headers.get_all("set-cookie").iter() {
        if let Ok(s) = v.to_str() {
            if let Some(rest) = s.trim().strip_prefix(&prefix) {
                let val = rest.split(';').next().unwrap_or("").trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// 从响应头解析极验数据（x-rpc-aigis）。
fn extract_aigis(headers: &reqwest::header::HeaderMap) -> Option<Value> {
    let aigis = headers.get("x-rpc-aigis")?;
    let v = aigis.to_str().ok()?;
    if v.is_empty() || v == "null" {
        return None;
    }
    serde_json::from_str(v).ok()
}

/// 米哈游：发送短信验证码。
/// 返回 Some(aigis) 表示触发人机验证（前端渲染极验后带 aigis 重试）；None 表示已发送。
pub async fn mihoyo_send_sms(
    device_id: &str,
    device_fp: &str,
    phone: &str,
    aigis: &str,
) -> Result<Option<Value>, String> {
    let enc_mobile = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, phone)?;
    let enc_area = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, "+86")?;
    let body = json!({ "mobile": enc_mobile, "area_code": enc_area }).to_string();
    let mut headers = passport_web_headers(device_id, device_fp);
    if !aigis.is_empty() {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(aigis) {
            headers.insert("x-rpc-aigis", v);
        }
        headers.insert("x-rpc-aigis_v4", reqwest::header::HeaderValue::from_static("true"));
    }
    let resp = crate::http::client()
        .post(constants::MIYOUSHE_SEND_SMS_URL)
        .headers(headers)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let resp_headers = resp.headers().clone();
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if json.get("retcode").and_then(|v| v.as_i64()) == Some(0) {
        return Ok(None);
    }
    // 触发人机验证：响应头含 x-rpc-aigis。
    if let Some(a) = extract_aigis(&resp_headers) {
        return Ok(Some(a));
    }
    let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("发送失败");
    Err(format!("发送验证码失败: {msg}"))
}

/// 米哈游：验证码登录。
pub async fn mihoyo_login_sms(
    device_id: &str,
    device_fp: &str,
    phone: &str,
    code: &str,
    aigis: &str,
) -> Result<CredOutcome, String> {
    let enc_mobile = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, phone)?;
    let enc_area = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, "+86")?;
    let body = json!({
        "mobile": enc_mobile,
        "area_code": enc_area,
        "action_type": "login_by_mobile_captcha",
        "captcha": code,
    })
    .to_string();
    let mut headers = passport_web_headers(device_id, device_fp);
    if !aigis.is_empty() {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(aigis) {
            headers.insert("x-rpc-aigis", v);
        }
        headers.insert("x-rpc-aigis_v4", reqwest::header::HeaderValue::from_static("true"));
    }
    let resp = crate::http::client()
        .post(constants::MIYOUSHE_LOGIN_SMS_URL)
        .headers(headers)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let resp_headers = resp.headers().clone();
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    // web 版登录：凭证在 Set-Cookie 中（非 body）。输出全部 Set-Cookie 供诊断。
    let all_cookies: Vec<String> = resp_headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
        .collect();
    log::info!("[短信登录] Set-Cookie: {}", all_cookies.join(" | "));
    log::info!("[短信登录] body: {}", &text[..text.len().min(300)]);
    let json: Value = serde_json::from_str(&text)
        .map_err(|_| format!("响应非 JSON: {}", &text[..text.len().min(200)]))?;
    // 极验触发。
    if let Some(a) = extract_aigis(&resp_headers) {
        return Ok(CredOutcome::NeedCaptcha(a));
    }
    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("登录失败");
        return Err(msg.to_string());
    }
    // web 版成功：从 Cookie 提取 stoken/mid/uid。
    let stoken = cookie_value(&resp_headers, "stoken")
        .or_else(|| cookie_value(&resp_headers, "stoken_v2"))
        .unwrap_or_default();
    let mid = cookie_value(&resp_headers, "ltmid")
        .or_else(|| cookie_value(&resp_headers, "ltmid_v2"))
        .or_else(|| cookie_value(&resp_headers, "mid"))
        .or_else(|| cookie_value(&resp_headers, "ltuid"))
        .or_else(|| cookie_value(&resp_headers, "account_id"))
        .unwrap_or_default();
    let uid = cookie_value(&resp_headers, "ltuid")
        .or_else(|| cookie_value(&resp_headers, "ltuid_v2"))
        .or_else(|| cookie_value(&resp_headers, "account_id"))
        .unwrap_or_default();
    if stoken.is_empty() {
        return Err(format!(
            "短信登录成功但未取得 stoken（Cookie: {}）",
            all_cookies.join(" | ")
        ));
    }
    Ok(CredOutcome::MiyousheOk {
        uid: if uid.is_empty() { mid.clone() } else { uid },
        stoken,
        mid,
    })
}

/// 米哈游：密码登录。
pub async fn mihoyo_login_pwd(
    device_id: &str,
    device_fp: &str,
    account: &str,
    password: &str,
    aigis: &str,
) -> Result<CredOutcome, String> {
    let enc_account = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, account)?;
    let enc_pwd = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, password)?;
    let body = json!({ "account": enc_account, "password": enc_pwd }).to_string();
    let mut headers = dsprod_headers(device_id, device_fp);
    if !aigis.is_empty() {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(aigis) {
            headers.insert("x-rpc-aigis", v);
        }
        headers.insert("x-rpc-aigis_v4", reqwest::header::HeaderValue::from_static("true"));
    }
    let resp = crate::http::client()
        .post(constants::MIYOUSHE_LOGIN_PWD_URL)
        .headers(headers)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let resp_headers = resp.headers().clone();
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    let json: Value = serde_json::from_str(&text)
        .map_err(|_| format!("响应非 JSON: {}", &text[..text.len().min(200)]))?;
    parse_mihoyo_login(&json, &resp_headers)
}

/// B站：获取 Web 登录 RSA 公钥 + 盐。
async fn bili_web_key() -> Result<(String, String), String> {
    let resp = crate::http::client()
        .get(constants::BILI_WEB_KEY_URL)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) BiliLogin/1.0")
        .send()
        .await
        .map_err(|e| format!("获取 B站公钥失败: {e}"))?;
    let json: Value = resp.json().await.map_err(|e| format!("B站公钥响应解析失败: {e}"))?;
    let key = json["data"]["key"].as_str().unwrap_or_default().to_string();
    let salt = json["data"]["hash"].as_str().unwrap_or_default().to_string();
    if key.is_empty() {
        return Err("B站公钥为空".to_string());
    }
    Ok((key, salt))
}

/// B站：发送短信验证码。
pub async fn bili_send_sms(phone: &str) -> Result<(), String> {
    // B站短信发送走 geeTest 流程，需先完成验证码；此处直连会触发风控。
    let (key, salt) = bili_web_key().await?;
    let enc = crypto::rsa_encrypt_b64(&key, &format!("{salt}{phone}"))?;
    let body = format!("tel={enc}&cid=86&source=main-fe-header");
    let resp = crate::http::client()
        .post(constants::BILI_SMS_SEND_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) BiliLogin/1.0")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let json: Value = resp.json().await.map_err(|e| format!("响应解析失败: {e}"))?;
    if json.get("code").and_then(|v| v.as_i64()) == Some(0) {
        return Ok(());
    }
    let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("发送失败");
    Err(format!("B站发送验证码失败: {msg}"))
}

/// B站：验证码登录。
pub async fn bili_login_sms(
    phone: &str,
    code: &str,
    captcha_key: &str,
) -> Result<CredOutcome, String> {
    let _ = (phone, code, captcha_key);
    Err("B站验证码登录待接入（需先完成极验）".to_string())
}

/// B站：密码登录。
pub async fn bili_login_pwd(
    account: &str,
    password: &str,
    captcha_key: &str,
) -> Result<CredOutcome, String> {
    let _ = (account, password, captcha_key);
    Err("B站密码登录待接入（需先完成极验）".to_string())
}

/// 供上层记录使用时间（保留接口）。
pub fn touch_now() -> i64 {
    now_sec()
}
