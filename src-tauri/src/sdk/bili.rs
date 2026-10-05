//! B 站 SDK：TV 扫码登录（passport-tv-login）+ 游戏侧 user.info 查询。
//!
//! 扫码流程：申请 auth_code 并生成二维码 → 用户用 B 站 App 扫码确认 →
//! 轮询 poll 直接取得 `access_token`（即游戏 SDK 的 access_key）与 mid。

use serde::Serialize;
use serde_json::Value;

use crate::constants;
use crate::crypto;

fn now_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 把 `Value` 标量转成字符串（复刻 Python `f"{v}"`）。
fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 复刻 `bsgamesdk.setSign`：覆盖 timestamp/client_timestamp，生成 form 字符串。
/// 依赖 `serde_json` 的 preserve_order 以保持字段插入顺序。
pub fn set_sign(data: &mut Value) -> String {
    let ts = now_sec();
    data["timestamp"] = Value::from(ts);
    data["client_timestamp"] = Value::from(ts);

    let mut data2 = String::new();
    let mut sign_raw = String::new();

    if let Value::Object(obj) = data {
        // 顺序遍历生成 form（pwd 双写：先 quote 后原值）
        for (k, v) in obj.iter() {
            let val = scalar(v);
            if k == "pwd" {
                data2.push_str(&format!("pwd={}&", crypto::py_quote(&val)));
            }
            data2.push_str(&format!("{k}={val}&"));
        }
        // 排序拼接生成 sign 原文
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for k in keys {
            sign_raw.push_str(&scalar(&obj[k]));
        }
    }

    let sign = crypto::md5_hex(&format!("{sign_raw}{}", constants::BILI_SIGN_SALT));
    data2.push_str(&format!("sign={sign}"));
    data2
}

// ---------- TV 扫码登录 ----------

/// TV 扫码登录签名：参数按 key 排序拼 `k=v&...`（去尾 `&`）后追加盐做 MD5。
pub fn tv_sign(params: &[(&str, String)]) -> String {
    let mut sorted: Vec<(&str, String)> = params.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let raw: String = sorted
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    crypto::md5_hex(&format!("{raw}{}", constants::BILI_TV_APPSEC))
}

async fn post_form(url: &str, form: &str) -> Result<Value, String> {
    let resp = crate::http::client()
        .post(url)
        .header("User-Agent", "Mozilla/5.0 BH3Scanner")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form.to_string())
        .send()
        .await
        .map_err(|e| format!("B 站请求失败: {e}"))?;
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {e}"))?;
    serde_json::from_str(&text)
        .map_err(|_| format!("B 站响应非 JSON: {}", &text[..text.len().min(200)]))
}

fn tv_signed_form(extra: &[(&str, String)]) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("appkey", constants::BILI_TV_APPKEY.to_string()),
        ("local_id", "0".to_string()),
        ("ts", now_sec().to_string()),
    ];
    params.extend_from_slice(extra);
    let sign = tv_sign(&params);
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())))
        .append_pair("sign", &sign)
        .finish()
}

/// 扫码登录会话：二维码内容 + 轮询凭据。
#[derive(Debug, Clone, Serialize)]
pub struct BiliQrSession {
    pub qr_url: String,
    pub auth_code: String,
}

/// 轮询结果。
#[derive(Debug, Clone, Serialize)]
pub enum BiliQrStatus {
    /// 等待扫码 / 等待确认
    Waiting,
    /// 已扫码，待手机确认
    Scanned,
    /// 已确认：mid 即 B 站 uid，access_token 即游戏 access_key
    Confirmed { uid: String, access_key: String },
    /// 二维码已过期，需重新生成
    Expired,
}

/// 申请扫码登录二维码。
pub async fn qr_login_start() -> Result<BiliQrSession, String> {
    let form = tv_signed_form(&[]);
    let resp = post_form(constants::BILI_TV_AUTH_CODE_URL, &form).await?;
    if resp.get("code").and_then(|v| v.as_i64()) != Some(0) {
        let msg = resp
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("申请二维码失败");
        return Err(msg.to_string());
    }
    let qr_url = resp["data"]["url"]
        .as_str()
        .ok_or_else(|| "响应缺少二维码 url".to_string())?
        .to_string();
    let auth_code = resp["data"]["auth_code"]
        .as_str()
        .ok_or_else(|| "响应缺少 auth_code".to_string())?
        .to_string();
    Ok(BiliQrSession { qr_url, auth_code })
}

/// 轮询扫码状态（有效期 180 秒，过期返回 Expired）。
pub async fn qr_login_poll(auth_code: &str) -> Result<BiliQrStatus, String> {
    let form = tv_signed_form(&[("auth_code", auth_code.to_string())]);
    let resp = post_form(constants::BILI_TV_POLL_URL, &form).await?;
    match resp.get("code").and_then(|v| v.as_i64()) {
        Some(0) => {
            let uid = resp["data"]["mid"]
                .to_string()
                .trim_matches('"')
                .to_string();
            let access_key = resp["data"]["access_token"]
                .as_str()
                .ok_or_else(|| "登录响应缺少 access_token".to_string())?
                .to_string();
            Ok(BiliQrStatus::Confirmed { uid, access_key })
        }
        // 86038 已失效；86090 已扫码未确认；86101/86039 未扫描
        Some(86038) => Ok(BiliQrStatus::Expired),
        Some(86090) => Ok(BiliQrStatus::Scanned),
        Some(86101) | Some(86039) => Ok(BiliQrStatus::Waiting),
        Some(code) => {
            let msg = resp
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("未知错误");
            Err(format!("扫码登录失败（{code}）: {msg}"))
        }
        None => Err("B 站响应缺少 code 字段".to_string()),
    }
}

// ---------- 游戏侧用户信息 ----------

async fn send_bili_post(url: &str, data: &str) -> Result<Value, String> {
    let resp = crate::http::client()
        .post(url)
        .header("User-Agent", "Mozilla/5.0 BSGameSDK")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(data.to_string())
        .send()
        .await
        .map_err(|e| format!("B 站请求失败: {e}"))?;
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {e}"))?;
    serde_json::from_str(&text)
        .map_err(|_| format!("B 站响应非 JSON: {}", &text[..text.len().min(200)]))
}

/// 获取用户名（失败不影响登录，仅用于展示）。
pub async fn get_user_info(uid: &str, access_key: &str) -> Result<String, String> {
    let mut data: Value = serde_json::from_str(constants::BILI_USERINFO_PARAM)
        .map_err(|e| format!("userinfoParam 解析失败: {e}"))?;
    data["uid"] = Value::String(uid.to_string());
    data["access_key"] = Value::String(access_key.to_string());
    let form = set_sign(&mut data);
    let resp = send_bili_post(
        &format!("{}api/client/user.info", constants::BILI_LOGIN),
        &form,
    )
    .await?;
    Ok(resp
        .get("uname")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string())
}
