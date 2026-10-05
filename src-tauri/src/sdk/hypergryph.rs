//! 鹰角网络（森空岛）SDK：短信验证码登录 + 游戏扫码确认。
//!
//! 协议来源：yu402317/skland-scan（公开的森空岛扫码登录协议实现）。
//! 关键约束：
//! - 固定 UA（改动会被服务端拒绝）；
//! - 响应 {msg, status, type}，status==0 才算成功；
//! - 参数校验失败走 HTTP 400（响应无 status，只有 msg）。
//!
//! 与米哈游的差异：鹰角扫码需**用户先登录鹰角账号**（手机号+验证码 → token），
//! 再用该 token 确认游戏二维码。

use serde::Serialize;
use serde_json::{json, Value};

const AS_HOST: &str = "https://as.hypergryph.com";
/// 固定 UA（照抄 skland-scan，改动会被服务端拒绝）。
const SKLAND_UA: &str =
    "Skland/1.5.1 (com.hypergryph.skland; build:100501001; Android 33; ) Okhttp/4.11.0";

/// 鹰角账号令牌（一个账号一条记录）。
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct HypergryphToken {
    pub phone: String,
    /// 鹰角通行证 token（短信登录获得）。
    pub token: String,
    /// 所属游戏 id（如 "endfield"）；空表示旧数据。
    #[serde(default)]
    pub game: String,
    /// 用户备注名。
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub remark: String,
    pub last_login_succ: bool,
    #[serde(default)]
    pub last_used_at: i64,
}

fn now_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// POST JSON 到鹰角，返回解析后的 JSON。
async fn post_json(path: &str, body: &Value, token: Option<&str>) -> Result<Value, String> {
    let url = format!("{AS_HOST}{path}");
    let mut headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), SKLAND_UA.to_string()),
    ];
    if let Some(t) = token {
        headers.push(("cred".to_string(), t.to_string())); // 鹰角部分接口用 cred 头
    }
    let resp = crate::http::post(&url, headers, &body.to_string())
        .await
        .map_err(|e| format!("鹰角请求失败: {e}"))?;
    // 参数校验失败走 HTTP 400：响应无 status，只有 msg。
    let json: Value = serde_json::from_str(&resp.body).map_err(|_| {
        format!(
            "鹰角响应非 JSON: {}",
            &resp.body[..resp.body.len().min(200)]
        )
    })?;
    if !(200..300).contains(&resp.status) {
        let msg = json
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("请求失败");
        return Err(format!("鹰角接口错误({}): {msg}", resp.status));
    }
    Ok(json)
}

/// 检查响应 status 字段（0 成功）。返回 Err(消息) 表示失败。
fn check_status(json: &Value, ctx: &str) -> Result<(), String> {
    let status = json.get("status").and_then(|v| v.as_i64()).unwrap_or(-1);
    if status == 0 {
        return Ok(());
    }
    let msg = json
        .get("msg")
        .and_then(|v| v.as_str())
        .unwrap_or("未知错误");
    let hint = match status {
        3 => "（token 已过期，请重新登录）",
        100 => "（二维码已过期）",
        101 => "（手机号或验证码错误）",
        102 => "（验证码发送过于频繁）",
        _ => "",
    };
    Err(format!("{ctx}: {msg}{hint}"))
}

/// 发送短信验证码（type=2 登录）。
pub async fn send_phone_code(phone: &str) -> Result<(), String> {
    let body = json!({ "phone": phone, "type": 2 });
    let json = post_json("/general/v1/send_phone_code", &body, None).await?;
    check_status(&json, "发送验证码失败")
}

/// 短信验证码登录，返回 token。
pub async fn token_by_phone_code(phone: &str, code: &str) -> Result<String, String> {
    let body = json!({ "phone": phone, "code": code });
    let json = post_json("/user/auth/v2/token_by_phone_code", &body, None).await?;
    check_status(&json, "登录失败")?;
    json.get("data")
        .and_then(|d| d.get("token"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "登录响应缺少 token".to_string())
}

/// 提交扫码（scanId + token）。
pub async fn scan_login(scan_id: &str, token: &str) -> Result<(), String> {
    let body = json!({ "scanId": scan_id, "token": token });
    let json = post_json("/user/info/v1/scan_login", &body, None).await?;
    check_status(&json, "提交扫码失败")
}

/// 确认授权。
pub async fn update_scan_status(scan_id: &str, token: &str) -> Result<(), String> {
    let body = json!({ "scanId": scan_id, "token": token });
    let json = post_json("/user/info/v1/update_scan_status", &body, None).await?;
    check_status(&json, "确认授权失败")
}

/// 完整确认流程：提交扫码 + 确认授权。
pub async fn confirm_scan(scan_id: &str, token: &str) -> Result<(), String> {
    scan_login(scan_id, token).await?;
    update_scan_status(scan_id, token).await
}

/// 供上层记录使用时间。
pub fn touch_now() -> i64 {
    now_sec()
}
