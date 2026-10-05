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
fn dsprod_headers(device_id: &str) -> reqwest::header::HeaderMap {
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
    set("x-rpc-device_fp", constants::DSPROD_DEVICE_FP_DEFAULT);
    set("x-rpc-device_name", "崩坏3扫码器-PC");
    set("x-rpc-device_model", "Windows");
    set("x-rpc-sys_version", "Windows 10");
    set("x-rpc-game_biz", constants::DSPROD_GAME_BIZ);
    set("x-rpc-app_version", constants::DSPROD_APP_VERSION);
    set("x-rpc-sdk_version", constants::DSPROD_SDK_VERSION);
    set("x-rpc-account_version", constants::DSPROD_ACCOUNT_VERSION);
    h
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

/// 米哈游：发送短信验证码。
/// aigis 为空时若触发风控会返回 NeedCaptcha（但此接口 Result<(),_> 无法表达，故触发时报错引导）。
pub async fn mihoyo_send_sms(device_id: &str, phone: &str, aigis: &str) -> Result<(), String> {
    let enc_mobile = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, phone)?;
    let enc_area = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, "+86")?;
    let body = json!({ "mobile": enc_mobile, "area_code": enc_area }).to_string();
    let mut headers = dsprod_headers(device_id);
    if !aigis.is_empty() {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(aigis) {
            headers.insert("x-rpc-aigis", v);
        }
    }
    let resp = crate::http::client()
        .post(constants::MIYOUSHE_SEND_SMS_URL)
        .headers(headers)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if json.get("retcode").and_then(|v| v.as_i64()) == Some(0) {
        return Ok(());
    }
    let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("发送失败");
    Err(format!("发送验证码失败({status}): {msg}"))
}

/// 米哈游：验证码登录。
pub async fn mihoyo_login_sms(
    device_id: &str,
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
    let mut headers = dsprod_headers(device_id);
    if !aigis.is_empty() {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(aigis) {
            headers.insert("x-rpc-aigis", v);
        }
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
    let json: Value = serde_json::from_str(&text)
        .map_err(|_| format!("响应非 JSON: {}", &text[..text.len().min(200)]))?;
    parse_mihoyo_login(&json, &resp_headers)
}

/// 米哈游：密码登录。
pub async fn mihoyo_login_pwd(
    device_id: &str,
    account: &str,
    password: &str,
    aigis: &str,
) -> Result<CredOutcome, String> {
    let enc_account = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, account)?;
    let enc_pwd = crypto::rsa_encrypt_b64(constants::MIYOUSHE_RSA_PUBLIC_KEY, password)?;
    let body = json!({ "account": enc_account, "password": enc_pwd }).to_string();
    let mut headers = dsprod_headers(device_id);
    if !aigis.is_empty() {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(aigis) {
            headers.insert("x-rpc-aigis", v);
        }
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
