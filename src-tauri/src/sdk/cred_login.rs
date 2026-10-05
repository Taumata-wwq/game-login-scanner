//! 渠道账号登录（米哈游通行证 / B站）：短信验证码 + 密码。
//!
//! ⚠️ 状态：**API 骨架 + UI 已就绪，但极验（geetest）人机验证与 RSA 公钥尚未接入**，
//! 实际登录链路尚不可用。接入极验 Web SDK 并填入真实 RSA 公钥后即可启用。
//!
//! 米哈游：createLoginCaptcha / loginByMobileCaptcha / loginByPassword（均需 x-rpc-aigis）。
//! B站：sms/send + login/sms + web/login（均需 geetest 验证）。

use serde_json::{json, Value};

/// 登录结果：成功返回账号令牌；需要人机验证返回 aigis 数据。
pub enum CredOutcome {
    /// 米哈游官服登录成功（stoken + mid + uid）。
    MiyousheOk {
        uid: String,
        stoken: String,
        mid: String,
    },
    /// B站登录成功（uid + access_key）。
    BiliOk { uid: String, access_key: String },
    /// 需要人机验证（返回给前端渲染）。
    NeedCaptcha(Value),
}

/// 米哈游：发送短信验证码（需 aigis，无则返回 NeedCaptcha）。
pub async fn mihoyo_send_sms(phone: &str, aigis: &str) -> Result<(), String> {
    let _body = json!({
        "mobile": phone,
        "area_code": "+86",
        "aigis": aigis,
    })
    .to_string();
    let _ = (phone, aigis);
    // 骨架：实际需处理 x-rpc-aigis 响应头与 retcode。
    Err("米哈游短信登录尚未接入（缺极验与 RSA 公钥）".to_string())
}

/// 米哈游：验证码登录。
pub async fn mihoyo_login_sms(
    _phone: &str,
    _code: &str,
    _aigis: &str,
) -> Result<CredOutcome, String> {
    Err("米哈游验证码登录尚未接入（缺极验与 RSA 公钥）".to_string())
}

/// 米哈游：密码登录。
pub async fn mihoyo_login_pwd(
    _account: &str,
    _password: &str,
    _aigis: &str,
) -> Result<CredOutcome, String> {
    Err("米哈游密码登录尚未接入（缺极验与 RSA 公钥）".to_string())
}

/// B站：发送短信验证码。
pub async fn bili_send_sms(_phone: &str) -> Result<(), String> {
    Err("B站短信登录尚未接入（缺极验）".to_string())
}

/// B站：验证码登录。
pub async fn bili_login_sms(
    _phone: &str,
    _code: &str,
    _captcha_key: &str,
) -> Result<CredOutcome, String> {
    Err("B站验证码登录尚未接入（缺极验）".to_string())
}

/// B站：密码登录。
pub async fn bili_login_pwd(
    _account: &str,
    _password: &str,
    _captcha_key: &str,
) -> Result<CredOutcome, String> {
    Err("B站密码登录尚未接入（缺极验）".to_string())
}
