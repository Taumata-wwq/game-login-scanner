//! 米哈游 B 服 SDK：HMAC-SHA256 makeSign + verify + scanCheck/scanConfirm + OA 分发。
//!
//! 版本号与 OA 分发信息按进程缓存，避免重复请求；分发内容过短视为获取失败。

use std::sync::RwLock;

use serde::Serialize;
use serde_json::{json, Value};

use crate::constants;
use crate::crypto;

/// 进程内缓存：崩坏3 版本号。
static BH_VER_CACHE: RwLock<Option<String>> = RwLock::new(None);

/// 进程内缓存：OA 分发串（按版本号缓存；版本变化时自动失效重取）。
/// dispatch 是稳定的 base64 串，单次会话内几乎不变，缓存可省一个 RTT。
static OA_DISPATCH_CACHE: RwLock<Option<(String, String)>> = RwLock::new(None);

/// 进程内缓存：当前 B 服账号的 bh_info（uid 匹配才复用）。
/// 启动 / 切换账号时预校验预热，扫描命中二维码后确认零等待。
static BH_INFO_CACHE: RwLock<Option<(String, BhInfo)>> = RwLock::new(None);

/// 校验 B 服账号并换取 bh_info（带缓存：同一 uid 复用，直到切换账号）。
pub async fn verify_cached(uid: &str, access_key: &str) -> Result<BhInfo, String> {
    if let Ok(c) = BH_INFO_CACHE.read() {
        if let Some((cached_uid, info)) = c.as_ref() {
            if cached_uid == uid {
                return Ok(info.clone());
            }
        }
    }
    let info = verify(uid, access_key).await?;
    if let Ok(mut c) = BH_INFO_CACHE.write() {
        *c = Some((uid.to_string(), info.clone()));
    }
    Ok(info)
}

fn now_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// HMAC-SHA256 签名（米哈游 B 服）。
pub fn bh3_sign(data: &str) -> String {
    crypto::hmac_sha256_hex(constants::MIHOYO_SIGN_KEY, data)
}

/// 复刻 `makeSign`：排序拼接 -> HMAC -> 写回 sign 字段，返回签名。
pub fn make_sign(data: &mut Value) -> String {
    let mut data2 = String::new();
    if let Value::Object(obj) = data {
        let mut keys: Vec<String> = obj.keys().cloned().collect();
        keys.sort();
        for k in keys {
            if k == "sign" {
                continue;
            }
            let v = value_to_str(&obj[&k]);
            data2.push_str(&format!("{k}={v}&"));
        }
    }
    data2 = data2.trim_end_matches('&').replace(' ', "");
    let sign = bh3_sign(&data2);
    data["sign"] = Value::String(sign.clone());
    sign
}

fn value_to_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

async fn send_post_json_str(url: &str, body: &str) -> Result<Value, String> {
    let resp = crate::http::post(url, Vec::new(), body)
        .await
        .map_err(|e| format!("米哈游请求失败: {e}"))?;
    serde_json::from_str(&resp.body).map_err(|_| {
        format!(
            "米哈游响应非 JSON: {}",
            &resp.body[..resp.body.len().min(200)]
        )
    })
}

async fn send_get_raw(url: &str) -> Result<String, String> {
    let resp = crate::http::get(url, Vec::new())
        .await
        .map_err(|e| format!("GET 请求失败: {e}"))?;
    Ok(resp.body)
}

/// 获取崩坏3 当前版本号；失败时回退进程内缓存，无缓存则报错。
async fn get_bh_ver() -> Result<String, String> {
    let text = send_get_raw(constants::BH3_VERSION_URL).await?;
    let v: Value = serde_json::from_str(&text).map_err(|_| "版本接口返回非 JSON".to_string())?;
    if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
        let ver = ver.to_string();
        if let Ok(mut cache) = BH_VER_CACHE.write() {
            *cache = Some(ver.clone());
        }
        return Ok(ver);
    }
    if let Some(ver) = BH_VER_CACHE.read().ok().and_then(|c| c.clone()) {
        return Ok(ver);
    }
    Err("版本接口缺少 version 字段".to_string())
}

/// 获取 OA 分发信息（返回 dispatch 原始文本）。
///
/// 首选 hellocraft v3 接口（[仅B服]PC扫码器 v1.4.7 同款）：返回与当前游戏版本
/// 实时匹配的 dispatch。固化的预置串是旧游戏版本数据，游戏资源更新后即过期，
/// 会导致游戏端资源版本不匹配、每次登录下载数百 MB 差异资源。
/// 回退链：hellocraft v3 -> 预置串 -> query_gameserver。
///
/// 请求格式（逆向 v1.4.7 得到）：URL 带 version 与 t 查询参数，头部带
/// x-req-code/x-req-name/x-req-openid/x-req-version，以及 x-req-sign =
/// HMAC-SHA256(MIHOYO_SIGN_KEY, signParam)，signParam 见 fetch_oa_hellocraft。
pub async fn get_oa_server(openid: &str) -> Result<String, String> {
    let bh_ver = match get_bh_ver().await {
        Ok(v) => v,
        Err(e) => return Err(format!("获取崩坏3版本号失败: {e}")),
    };
    // 命中缓存（版本一致）时直接返回。
    if let Ok(c) = OA_DISPATCH_CACHE.read() {
        if let Some((cached_ver, dispatch)) = c.as_ref() {
            if cached_ver == &bh_ver {
                return Ok(dispatch.clone());
            }
        }
    }
    // 方式一（首选）：hellocraft v3 接口。
    match fetch_oa_hellocraft(&bh_ver, openid).await {
        Ok(d) if d.chars().count() >= 100 => {
            log::info!("使用 hellocraft OA dispatch: len={}", d.len());
            if let Ok(mut c) = OA_DISPATCH_CACHE.write() {
                *c = Some((bh_ver.clone(), d.clone()));
            }
            return Ok(d);
        }
        Ok(d) => log::warn!("hellocraft OA 返回过短（{}），回退", d.chars().count()),
        Err(e) => log::warn!("hellocraft OA 调用失败: {e}，回退"),
    }
    // 方式二（回退）：按版本预置的分发串（可能过期）。
    if let Some(dispatch) = constants::preset_dispatch(&bh_ver) {
        if let Ok(mut c) = OA_DISPATCH_CACHE.write() {
            *c = Some((bh_ver, dispatch.to_string()));
        }
        return Ok(dispatch.to_string());
    }
    // 方式三（回退）：query_gameserver 动态查询（安卓标签）。
    let url = format!(
        "https://outer-dp-bb01.bh3.com/query_gameserver?version={}_gf_android_bilibili&token={}",
        bh_ver,
        constants::OA_TOKEN_DEFAULT
    );
    let dispatch = send_get_raw(&url).await?;
    if dispatch.trim().chars().count() < 10 {
        return Err("获取 OA 服务器信息失败，请检查网络后重试".to_string());
    }
    if let Ok(mut c) = OA_DISPATCH_CACHE.write() {
        *c = Some((bh_ver, dispatch.clone()));
    }
    Ok(dispatch)
}

/// hellocraft v3 OA 查询（v1.4.7 同款）。返回 JSON 的 data 字段（dispatch 原文）。
async fn fetch_oa_hellocraft(bh_ver: &str, openid: &str) -> Result<String, String> {
    let ver_tag = format!("{bh_ver}_gf_android_bilibili");
    let ts = now_sec();
    let url = format!("{}?version={ver_tag}&t={ts}", constants::BH3_OA_DISPATCH_URL);
    let sign_param = format!(
        "x-req-code=80&x-req-name=pc-1.4.7:80&x-req-openid={openid}&x-req-version={ver_tag}"
    );
    let sign = bh3_sign(&sign_param);
    let headers: crate::http::Headers = vec![
        ("x-req-code".to_string(), "80".to_string()),
        ("x-req-name".to_string(), "pc-1.4.7:80".to_string()),
        ("x-req-openid".to_string(), openid.to_string()),
        ("x-req-version".to_string(), ver_tag.clone()),
        ("x-req-sign".to_string(), sign),
    ];
    let resp = crate::http::get(&url, headers)
        .await
        .map_err(|e| format!("hellocraft 请求失败: {e}"))?;
    let v: Value = serde_json::from_str(&resp.body)
        .map_err(|_| format!("hellocraft 响应非 JSON: {}", &resp.body[..resp.body.len().min(120)]))?;
    if v.get("retcode").and_then(|x| x.as_i64()) != Some(0) {
        let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("未知错误");
        return Err(format!("hellocraft 返回错误: {msg}"));
    }
    v.get("data")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "hellocraft 响应缺少 data".to_string())
}

/// B 服 bh_info（由 verify 得到）。
#[derive(Debug, Clone, Serialize)]
pub struct BhInfo {
    pub open_id: String,
    pub combo_id: String,
    pub combo_token: String,
}

/// 校验 B 站账号并换取 bh_info。
/// 注意：app_id/channel_id 需为数字、uid 需为数字，否则服务器返回「渠道错误」(-107)。
pub async fn verify(uid: &str, access_key: &str) -> Result<BhInfo, String> {
    let uid_num = uid.parse::<i64>().unwrap_or_default();
    let mut body = json!({
        "device": "0000000000000000",
        "app_id": 1,
        "channel_id": 14,
        "data": {},
        "sign": ""
    });
    let data = if uid_num != 0 {
        json!({ "uid": uid_num, "access_key": access_key })
    } else {
        json!({ "uid": uid, "access_key": access_key })
    };
    body["data"] = Value::String(data.to_string());
    make_sign(&mut body);
    let post_body = body.to_string().replace(' ', "");
    let feedback = send_post_json_str(constants::MIHOYO_VERIFY_URL, &post_body).await?;
    if feedback.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = feedback
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("verify 失败");
        return Err(msg.to_string());
    }
    let data = feedback
        .get("data")
        .ok_or_else(|| "verify 返回缺少 data".to_string())?;
    Ok(BhInfo {
        open_id: data
            .get("open_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        combo_id: data
            .get("combo_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        combo_token: data
            .get("combo_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// 扫码确认（scanCheck -> scanConfirm）。
/// `biz_key` 来自二维码（如 `bh3_cn`），决定端点前缀。
pub async fn scan_confirm(
    bh_info: &BhInfo,
    ticket: &str,
    biz_key: &str,
    display_name: &str,
    app_id: i64,
) -> Result<(), String> {
    // scanCheck
    let mut check = json!({
        "app_id": app_id.to_string(),
        "device": "0000000000000000",
        "ticket": ticket,
        "ts": now_sec(),
        "sign": ""
    });
    make_sign(&mut check);
    let check_body = check.to_string().replace(' ', "");
    let feedback = send_post_json_str(&constants::panda_scan_url(biz_key), &check_body).await?;
    if feedback.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = feedback
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("二维码可能已过期");
        return Err(msg.to_string());
    }

    // scanConfirm
    let dispatch = get_oa_server(&bh_info.open_id).await?;
    // 调试：记录 dispatch 来源特征（长度 + 首尾），用于排查资源下载问题。
    log::info!(
        "OA dispatch: len={} head={} tail={}",
        dispatch.len(),
        &dispatch[..dispatch.len().min(32)],
        &dispatch[dispatch.len().saturating_sub(16)..]
    );

    // 载荷对齐参考实现（scannerHelper / bh3_login_simulation）：
    // ext.data.accountID = open_id（**不要**留空，也不要用 MHY_Scanner 的 `c` 字段，
    // 该写法在当前服务端会导致游戏端拿不到账号、无法进入游戏）。
    let scan_data = json!({
        "accountType": "2",
        "accountID": bh_info.open_id,
        "accountToken": bh_info.combo_token,
        "dispatch": Value::String(dispatch),
    });
    let scan_ext = json!({ "data": scan_data });

    let scan_raw = json!({
        "heartbeat": false,
        "open_id": bh_info.open_id,
        "device_id": "0000000000000000",
        "app_id": app_id.to_string(),
        "channel_id": "14",
        "combo_token": bh_info.combo_token,
        "asterisk_name": display_name,
        "combo_id": bh_info.combo_id,
        "account_type": "2"
    });
    let scan_payload = json!({
        "raw": scan_raw.to_string(),
        "proto": "Combo",
        "ext": scan_ext.to_string(),
    });

    let mut result = json!({
        "device": "0000000000000000",
        "app_id": app_id,
        "ts": now_sec(),
        "ticket": ticket,
        "payload": scan_payload,
        "sign": ""
    });
    make_sign(&mut result);
    let confirm_body = result.to_string().replace(' ', "");
    // 调试：记录实际发出的 confirm 请求体（排查游戏端资源下载问题）。
    log::info!("scanConfirm 请求体: {confirm_body}");
    let fb = send_post_json_str(&constants::panda_confirm_url(biz_key), &confirm_body).await?;
    if fb.get("retcode").and_then(|v| v.as_i64()) != Some(0) {
        let msg = fb
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("扫码失败");
        return Err(msg.to_string());
    }
    Ok(())
}
