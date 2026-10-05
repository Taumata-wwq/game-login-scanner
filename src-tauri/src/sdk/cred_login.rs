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
    set("x-rpc-device_name", "游戏扫码器-PC");
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
    // 登录步骤用 App 协议（与发短信的通行证 Web 协议不同）：App 版接口返回 body 中的
    // stoken（data.token.token），Web 版只返回 Cookie（无 stoken）。
    // 参考 bh3_login_simulation：登录不传 aigis（aigis 仅用于发短信）。
    let headers = dsprod_headers(device_id, device_fp);
    let _ = aigis;
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
    // 极验触发。
    if let Some(a) = extract_aigis(&resp_headers) {
        return Ok(CredOutcome::NeedCaptcha(a));
    }
    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("登录失败");
        return Err(msg.to_string());
    }
    // app 版成功：stoken 在 body 的 data.token.token；web 版在 Set-Cookie。
    // 两者都尝试，body 优先。
    let data = &json["data"];
    let body_stoken = data["token"]["token"].as_str().unwrap_or_default().to_string();
    let body_mid = data["user_info"]["mid"].as_str().unwrap_or_default().to_string();
    let body_uid = data["user_info"]["aid"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| data["user_info"]["aid"].as_i64().map(|n| n.to_string()))
        .unwrap_or_default();
    let stoken = if !body_stoken.is_empty() {
        body_stoken
    } else {
        cookie_value(&resp_headers, "stoken")
            .or_else(|| cookie_value(&resp_headers, "stoken_v2"))
            .unwrap_or_default()
    };
    let mid = if !body_mid.is_empty() {
        body_mid
    } else {
        cookie_value(&resp_headers, "ltmid")
            .or_else(|| cookie_value(&resp_headers, "ltmid_v2"))
            .or_else(|| cookie_value(&resp_headers, "mid"))
            .or_else(|| cookie_value(&resp_headers, "ltuid"))
            .or_else(|| cookie_value(&resp_headers, "account_id"))
            .unwrap_or_default()
    };
    let uid = if !body_uid.is_empty() {
        body_uid
    } else {
        cookie_value(&resp_headers, "ltuid")
            .or_else(|| cookie_value(&resp_headers, "ltuid_v2"))
            .or_else(|| cookie_value(&resp_headers, "account_id"))
            .unwrap_or_default()
    };
    if stoken.is_empty() {
        return Err("短信登录成功但未取得 stoken".to_string());
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

/// 把键值对编码为 application/x-www-form-urlencoded 字符串。
fn url_encode(pairs: &[(String, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .finish()
}

/// 解析 B站极验结果 JSON：{"token","challenge","validate","seccode"}。
pub fn bili_parse_captcha(captcha_key: &str) -> Result<(String, String, String, String), String> {
    let v: Value = serde_json::from_str(captcha_key)
        .map_err(|_| "B站极验结果格式错误（应为 JSON）".to_string())?;
    let g = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let token = g("token");
    let challenge = g("challenge");
    let validate = g("validate");
    let seccode = g("seccode");
    if token.is_empty() || challenge.is_empty() || validate.is_empty() || seccode.is_empty() {
        return Err("B站极验结果字段缺失".to_string());
    }
    Ok((token, challenge, validate, seccode))
}

/// 获取 B站设备指纹 buvid3/buvid4（新版登录风控必需，缺失报 86103）。
async fn bili_buvid() -> Result<(String, String), String> {
    let resp = crate::http::client()
        .get("https://api.bilibili.com/x/frontend/finger/spi")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .send()
        .await
        .map_err(|e| format!("获取 buvid 失败: {e}"))?;
    let text = resp.text().await.map_err(|e| format!("buvid 读取失败: {e}"))?;
    log::info!("[B站buvid] {}", &text[..text.len().min(400)]);
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let b3 = json["data"]["b_3"].as_str().unwrap_or_default().to_string();
    let b4 = json["data"]["b_4"].as_str().unwrap_or_default().to_string();
    Ok((b3, b4))
}

/// B站短信/登录通用请求头（含 buvid 设备指纹 Cookie + 设备请求信息头）。
async fn bili_headers_with_buvid() -> reqwest::header::HeaderMap {
    let mut h = bili_headers();
    if let Ok((b3, b4)) = bili_buvid().await {
        if !b3.is_empty() {
            // buvid3/buvid4 是登录风控必需的设备指纹；b_nut 为获取时间戳。
            let nut = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let cookie = format!("buvid3={b3}; buvid4={b4}; b_nut={nut}");
            if let Ok(v) = reqwest::header::HeaderValue::from_str(&cookie) {
                h.insert("Cookie", v);
            }
        }
    }
    h
}

/// B站短信/登录通用请求头。
fn bili_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut h = HeaderMap::new();
    let mut set = |k: &'static str, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    };
    set(
        "User-Agent",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    );
    set("Referer", "https://passport.bilibili.com/login");
    set("Origin", "https://passport.bilibili.com");
    // 登录页真实请求携带的设备/位置信息头（缺失可能触发风控）。
    set(
        "x-bili-device-req-json",
        "{\"platform\":\"web\",\"device\":\"pc\",\"spmid\":\"333.1228\",\"mobi_app\":\"web_cn\"}",
    );
    set("x-bili-locale-json", "{\"c_locale\":{\"language\":\"zh\",\"script\":\"Hans\"},\"always_translate\":true}");
    h
}

/// B站：申请极验验证任务（返回 gt/challenge/token）。
/// 短信与密码登录触发风控时都需要先完成极验（GT3）。
pub async fn bili_captcha() -> Result<Value, String> {
    let resp = crate::http::client()
        .get(constants::BILI_CAPTCHA_URL)
        .headers(bili_headers_with_buvid().await)
        .send()
        .await
        .map_err(|e| format!("B站极验申请失败: {e}"))?;
    let text = resp.text().await.map_err(|e| format!("B站极验响应读取失败: {e}"))?;
    log::info!("[B站极验申请] body: {}", &text[..text.len().min(800)]);
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if json.get("code").and_then(|v| v.as_i64()) != Some(0) {
        let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("极验申请失败");
        return Err(format!("B站极验申请失败: {msg}"));
    }
    // 结构：{ data: { geetest: { gt, challenge }, token } }
    let mut out = json["data"]["geetest"].clone();
    if let Some(token) = json["data"]["token"].as_str() {
        out["token"] = Value::String(token.to_string());
    }
    Ok(out)
}

/// B站：发送短信验证码。
/// captcha_key 非空时携带极验结果（validate/seccode），触发风控时返回 NeedCaptcha。
pub async fn bili_send_sms(phone: &str, captcha_key: &str) -> Result<Option<Value>, String> {
    // B站通行证 Web 短信登录风控极严（恒定返回 86103「请下载最新版本哔哩哔哩App」），
    // 无浏览器设备指纹（buvid_fp/bili_ticket）无法通过。B站登录请改用「密码登录」。
    let _ = (phone, captcha_key);
    Err("B站短信登录受风控限制暂不可用，请改用「密码登录」".to_string())
}

/// 从 B站登录响应提取凭证（cookie 优先，body 兜底）。
fn bili_extract_cookies(headers: &reqwest::header::HeaderMap, json: &Value) -> CredOutcome {
    // B站登录成功后 cookie_token/DedeUserID/access_token 在 Set-Cookie。
    let uid = cookie_value(headers, "DedeUserID")
        .or_else(|| json["data"]["mid"].as_i64().map(|n| n.to_string()))
        .or_else(|| json["data"]["uid"].as_i64().map(|n| n.to_string()))
        .unwrap_or_default();
    // 游戏侧用 access_token（body 的 token_info.access_token 或 cookies 的 bili_jct）。
    let access_key = json["data"]["token_info"]["access_token"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| cookie_value(headers, "bili_jct"))
        .unwrap_or_default();
    CredOutcome::BiliOk { uid, access_key }
}

/// B站：验证码登录。
pub async fn bili_login_sms(
    phone: &str,
    code: &str,
    captcha_key: &str,
) -> Result<CredOutcome, String> {
    // 首次调用（无极验结果）：先申请极验任务返回前端。
    if captcha_key.is_empty() {
        return Ok(CredOutcome::NeedCaptcha(bili_captcha().await?));
    }
    let (token, challenge, validate, seccode) = bili_parse_captcha(captcha_key)?;
    // 短信登录：cid=1 + tel(明文) + code + 极验四参数。
    let form = vec![
        ("cid".to_string(), "1".to_string()),
        ("tel".to_string(), phone.to_string()),
        ("code".to_string(), code.to_string()),
        ("token".to_string(), token),
        ("challenge".to_string(), challenge),
        ("validate".to_string(), validate),
        ("seccode".to_string(), seccode),
    ];
    let body = url_encode(&form);
    let resp = crate::http::client()
        .post(constants::BILI_SMS_LOGIN_URL)
        .headers(bili_headers_with_buvid().await)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let headers = resp.headers().clone();
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    let json: Value = serde_json::from_str(&text)
        .map_err(|_| format!("响应非 JSON: {}", &text[..text.len().min(200)]))?;
    let code_num = json.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code_num != 0 {
        let msg = json.get("message").and_then(|m| m.as_str()).unwrap_or("登录失败");
        return Err(format!("B站验证码登录失败: {msg}"));
    }
    Ok(bili_extract_cookies(&headers, &json))
}

/// B站：密码登录。走**游戏 SDK**（line1-sdk-center-login-sh.biligame.net，app_id=180），
/// 直接返回游戏 access_key，避开通行证 Web 登录的严苛风控（86103）。
/// captcha_key 为空时直接尝试登录；若服务端要求验证码则返回 NeedCaptcha
/// （含 gt/challenge/userid），前端完成极验后以 JSON {challenge,userid,validate} 重试。
pub async fn bili_login_pwd(
    account: &str,
    password: &str,
    captcha_key: &str,
) -> Result<CredOutcome, String> {
    // 解析极验结果（若有）。
    let cap = if captcha_key.is_empty() {
        None
    } else {
        let v: Value = serde_json::from_str(captcha_key)
            .map_err(|_| "B站极验结果格式错误".to_string())?;
        Some((
            v["challenge"].as_str().unwrap_or_default().to_string(),
            v["userid"].as_str().unwrap_or_default().to_string(),
            v["validate"].as_str().unwrap_or_default().to_string(),
        ))
    };
    let cap_ref = cap.as_ref().map(|(c, u, v)| (c.as_str(), u.as_str(), v.as_str()));
    let resp = crate::sdk::bili::bili_sdk_login(account, password, cap_ref).await?;
    log::info!("[B站SDK登录] {}", &resp.to_string()[..resp.to_string().len().min(600)]);
    // 成功：含 access_key。
    if let Some(ak) = resp.get("access_key").and_then(|v| v.as_str()) {
        if !ak.is_empty() {
            let uid = resp
                .get("uid")
                .map(|v| v.to_string().trim_matches('"').to_string())
                .unwrap_or_default();
            return Ok(CredOutcome::BiliOk {
                uid,
                access_key: ak.to_string(),
            });
        }
    }
    // 需要验证码：申请极验任务返回前端。
    let cap_data = crate::sdk::bili::bili_sdk_captcha().await?;
    let gt = cap_data.get("gt").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let challenge = cap_data.get("challenge").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let userid = cap_data.get("gt_user_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    Ok(CredOutcome::NeedCaptcha(json!({
        "gt": gt,
        "challenge": challenge,
        "userid": userid,
    })))
}
