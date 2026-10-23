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
fn dsprod_headers(device_id: &str, device_fp: &str) -> crate::http::Headers {
    let mut h: crate::http::Headers = Vec::new();
    let mut set = |k: &str, v: &str| {
        h.push((k.to_string(), crate::http::header_utf8(v)));
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
fn passport_web_headers(device_id: &str, device_fp: &str) -> crate::http::Headers {
    let mut h: crate::http::Headers = Vec::new();
    let mut set = |k: &str, v: &str| {
        h.push((k.to_string(), v.to_string()));
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
fn parse_mihoyo_login(json: &Value, headers: &crate::http::Headers) -> Result<CredOutcome, String> {
    // 极验：响应头 x-rpc-aigis。
    if let Some(v) = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-rpc-aigis"))
        .map(|(_, v)| v.as_str())
    {
        if !v.is_empty() && v != "null" {
            if let Ok(parsed) = serde_json::from_str::<Value>(v) {
                return Ok(CredOutcome::NeedCaptcha(parsed));
            }
        }
    }
    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = json
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("登录失败");
        return Err(msg.to_string());
    }
    let data = &json["data"];
    let token = data["token"]["token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
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
pub async fn fetch_device_fp(
    device_id: &str,
    seed_id: &str,
    seed_time: &str,
) -> Result<String, String> {
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
    let resp = crate::http::post(
        DEVICE_FP_URL,
        vec![("Content-Type".to_string(), "application/json".to_string())],
        &body,
    )
    .await
    .map_err(|e| format!("getFp 请求失败: {e}"))?;
    let json: Value =
        serde_json::from_str(&resp.body).map_err(|e| format!("getFp 响应解析失败: {e}"))?;
    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = json
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("未知错误");
        return Err(format!("getFp 失败: {msg}"));
    }
    json["data"]["device_fp"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "getFp 响应缺少 device_fp".to_string())
}

/// 从响应头 Set-Cookie 中解析指定 Cookie 字段的值。
fn cookie_value(headers: &crate::http::Headers, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    for s in headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.as_str())
    {
        if let Some(rest) = s.trim().strip_prefix(&prefix) {
            let val = rest.split(';').next().unwrap_or("").trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// 从响应头解析极验数据（x-rpc-aigis）。
fn extract_aigis(headers: &crate::http::Headers) -> Option<Value> {
    let v = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-rpc-aigis"))
        .map(|(_, v)| v.as_str())?;
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
    headers.push(("Content-Type".to_string(), "application/json".to_string()));
    if !aigis.is_empty() {
        headers.push(("x-rpc-aigis".to_string(), aigis.to_string()));
        headers.push(("x-rpc-aigis_v4".to_string(), "true".to_string()));
    }
    let resp = crate::http::post(constants::MIYOUSHE_SEND_SMS_URL, headers, &body)
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let json: Value = serde_json::from_str(&resp.body).unwrap_or(Value::Null);
    if json.get("retcode").and_then(|v| v.as_i64()) == Some(0) {
        return Ok(None);
    }
    // 触发人机验证：响应头含 x-rpc-aigis。
    if let Some(a) = extract_aigis(&resp.headers) {
        return Ok(Some(a));
    }
    let msg = json
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("发送失败");
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
    // 触发极验时（下方 extract_aigis 返回 NeedCaptcha）由前端渲染极验、带 aigis 重试；
    // 重试必须携带 x-rpc-aigis 头（与 mihoyo_login_pwd 一致），否则条件不变会无限
    // 返回 NeedCaptcha_aigis 被丢弃，构成死循环）。
    let mut headers = dsprod_headers(device_id, device_fp);
    headers.push(("Content-Type".to_string(), "application/json".to_string()));
    if !aigis.is_empty() {
        headers.push(("x-rpc-aigis".to_string(), aigis.to_string()));
        headers.push(("x-rpc-aigis_v4".to_string(), "true".to_string()));
    }
    let resp = crate::http::post(constants::MIYOUSHE_LOGIN_SMS_URL, headers, &body)
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let json: Value = serde_json::from_str(&resp.body)
        .map_err(|_| format!("响应非 JSON: {}", crate::http::excerpt(&resp.body, 200)))?;
    // 极验触发。
    if let Some(a) = extract_aigis(&resp.headers) {
        return Ok(CredOutcome::NeedCaptcha(a));
    }
    if json.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = json
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("登录失败");
        return Err(msg.to_string());
    }
    // app 版成功：stoken 在 body 的 data.token.token；web 版在 Set-Cookie。
    // 两者都尝试，body 优先。
    let data = &json["data"];
    let body_stoken = data["token"]["token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let body_mid = data["user_info"]["mid"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let body_uid = data["user_info"]["aid"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| data["user_info"]["aid"].as_i64().map(|n| n.to_string()))
        .unwrap_or_default();
    let stoken = if !body_stoken.is_empty() {
        body_stoken
    } else {
        cookie_value(&resp.headers, "stoken")
            .or_else(|| cookie_value(&resp.headers, "stoken_v2"))
            .unwrap_or_default()
    };
    let mid = if !body_mid.is_empty() {
        body_mid
    } else {
        cookie_value(&resp.headers, "ltmid")
            .or_else(|| cookie_value(&resp.headers, "ltmid_v2"))
            .or_else(|| cookie_value(&resp.headers, "mid"))
            .or_else(|| cookie_value(&resp.headers, "ltuid"))
            .or_else(|| cookie_value(&resp.headers, "account_id"))
            .unwrap_or_default()
    };
    let uid = if !body_uid.is_empty() {
        body_uid
    } else {
        cookie_value(&resp.headers, "ltuid")
            .or_else(|| cookie_value(&resp.headers, "ltuid_v2"))
            .or_else(|| cookie_value(&resp.headers, "account_id"))
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
    headers.push(("Content-Type".to_string(), "application/json".to_string()));
    if !aigis.is_empty() {
        headers.push(("x-rpc-aigis".to_string(), aigis.to_string()));
        headers.push(("x-rpc-aigis_v4".to_string(), "true".to_string()));
    }
    let resp = crate::http::post(constants::MIYOUSHE_LOGIN_PWD_URL, headers, &body)
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let json: Value = serde_json::from_str(&resp.body)
        .map_err(|_| format!("响应非 JSON: {}", crate::http::excerpt(&resp.body, 200)))?;
    parse_mihoyo_login(&json, &resp.headers)
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
        let v: Value =
            serde_json::from_str(captcha_key).map_err(|_| "B站极验结果格式错误".to_string())?;
        Some((
            v["challenge"].as_str().unwrap_or_default().to_string(),
            v["userid"].as_str().unwrap_or_default().to_string(),
            v["validate"].as_str().unwrap_or_default().to_string(),
        ))
    };
    let cap_ref = cap
        .as_ref()
        .map(|(c, u, v)| (c.as_str(), u.as_str(), v.as_str()));
    let resp = crate::sdk::bili::bili_sdk_login(account, password, cap_ref).await?;
    // 只记「code + message」。这里**绝不能**打印整个响应体：成功时它含 access_key
    // （B 站登录令牌），失败时也可能回显账号信息。日志会落到 %APPDATA% 文件并长期
    // 保留，打印整包等于把令牌明文持久化到磁盘。
    log::info!(
        "[B站SDK登录] code={} message={}",
        resp.get("code")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into()),
        crate::http::excerpt(
            resp.get("message").and_then(|v| v.as_str()).unwrap_or("-"),
            120
        )
    );
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
    // 未拿到 access_key：先判断服务端是否**真的**在要验证码。
    //
    // 这里是无条件 `bili_sdk_captcha` —— 于是任何「响应里没有 access_key」的
    // 情况都被当成「需要人机验证」：网络/TLS 失败、密码错误、风控拒绝……用户看到的
    // 就是「弹了个验证窗，填完还是登不进去」，而错误原因被彻底掩盖（真机反馈）。
    // 现在只有服务端给出验证码信号（业务码/字段，见 bili::bili_login_needs_captcha）
    // 才走极验；否则如实抛出服务端的 code/message。
    if !crate::sdk::bili::bili_login_needs_captcha(&resp) {
        let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        let msg = resp
            .get("message")
            .or_else(|| resp.get("msg"))
            .and_then(|v| v.as_str())
            .unwrap_or("登录被拒绝");
        // 如实抛出服务端结论（账号密码错 / 风控 / 频繁等），让用户看到真实原因。
        return Err(format!("B站登录失败（{code}）：{msg}"));
    }
    // 需要验证码：申请极验任务返回前端。
    let cap_data = crate::sdk::bili::bili_sdk_captcha().await?;
    let gt = cap_data
        .get("gt")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let challenge = cap_data
        .get("challenge")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let userid = cap_data
        .get("gt_user_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(CredOutcome::NeedCaptcha(json!({
        "gt": gt,
        "challenge": challenge,
        "userid": userid,
    })))
}
