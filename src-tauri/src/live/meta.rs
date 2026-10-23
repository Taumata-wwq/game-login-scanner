use serde_json::Value;

use super::bili::{Platform, BROWSER_UA};

/// 前端房间列表用的会话摘要。
#[derive(Clone, serde::Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub room: String,
    pub platform: String,
    pub w: u32,
    pub h: u32,
    /// 直播间标题（元数据，可能为空）。
    #[serde(default)]
    pub title: String,
    /// 主播昵称（元数据，可能为空）。
    #[serde(default)]
    pub nickname: String,
    /// 封面图 URL（元数据，可能为空）。
    #[serde(default)]
    pub cover: String,
    /// 房间号（元数据；B站真实房间号可能不同于短号）。
    #[serde(default)]
    pub room_id: String,
    /// 已开播时长文本（如「1:23:45」，可能为空）。
    #[serde(default)]
    pub duration: String,
}

/// 直播间元数据（标题/主播/封面/房间号/开播状态/开播时刻）。
#[derive(Default, Clone)]
pub struct RoomMeta {
    pub title: String,
    pub nickname: String,
    pub cover: String,
    pub room_id: String,
    /// 1 = 直播中，0 = 未开播。
    pub live_status: i64,
    /// 开播时刻（Unix 秒）；0 = 未知/未开播。
    pub live_start: i64,
}

/// 按平台拉取直播间元数据（供前端轮询刷新用）。
pub fn fetch_meta(platform: Platform, room: &str) -> RoomMeta {
    match platform {
        Platform::Bilibili => fetch_bili_meta(room),
        Platform::Douyin => {
            let m = crate::douyin::fetch_meta_sync(room);
            RoomMeta {
                title: m.title,
                nickname: m.nickname,
                cover: m.cover,
                room_id: m.room_id,
                live_status: m.live_status,
                live_start: m.live_start,
            }
        }
    }
}

/// 开播时长文本（供命令层格式化）。
pub fn duration_text(live_start: i64) -> String {
    format_duration(live_start)
}

/// 拉取元数据（**严格版**）：保留失败原因（网络/DNS、风控、房间号无效等），
/// 供监听器与诊断日志使用；普通调用走宽松版 `fetch_meta`。
pub fn fetch_meta_strict(platform: Platform, room: &str) -> Result<RoomMeta, String> {
    match platform {
        Platform::Bilibili => fetch_bili_meta_strict(room),
        // 抖音元数据链路自带签名与重试处理，宽松语义即可（失败返回默认）。
        Platform::Douyin => Ok(fetch_meta(Platform::Douyin, room)),
    }
}

/// 拉取 B站直播间元数据（get_info 接口，免登录）。失败返回默认（不阻塞连接）。
pub(super) fn fetch_bili_meta(room: &str) -> RoomMeta {
    fetch_bili_meta_strict(room).unwrap_or_default()
}

/// get_info 严格实现：任一环节失败都带原因返回。
pub(super) fn fetch_bili_meta_strict(room: &str) -> Result<RoomMeta, String> {
    let url = format!("https://api.live.bilibili.com/room/v1/Room/get_info?room_id={room}");
    let headers = vec![
        ("User-Agent".to_string(), BROWSER_UA.to_string()),
        (
            "Referer".to_string(),
            format!("https://live.bilibili.com/{room}"),
        ),
    ];
    let body = crate::http::request_sync("GET", &url, &headers, None)
        .map_err(|e| format!("get_info 请求失败: {e}"))?
        .body;
    let v: Value =
        serde_json::from_str(&body).map_err(|e| format!("get_info 响应解析失败: {e}"))?;
    let code = v["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let msg = v["msg"]
            .as_str()
            .or_else(|| v["message"].as_str())
            .unwrap_or("");
        // code=1 =「未找到该房间」——房间号确实不存在，属**终态**；
        // 其余非 0 多为风控/参数问题，属**可重试**的瞬时失败。
        // 两者必须可区分：前者要如实告诉用户「房间不存在」，后者不该误报。
        return Err(format!("get_info code={code}（{msg}）"));
    }
    let d = &v["data"];
    let room_id = d["room_id"]
        .as_i64()
        .map(|x| x.to_string())
        .unwrap_or_default();
    if room_id.is_empty() {
        return Err("get_info 响应缺少 room_id".to_string());
    }
    let live_status = d["live_status"].as_i64().unwrap_or(0);
    // user_cover 常为空 → 回退 keyframe（首帧缩略图）。
    let cover = {
        let uc = d["user_cover"].as_str().unwrap_or("");
        if !uc.is_empty() {
            uc.to_string()
        } else {
            d["keyframe"].as_str().unwrap_or("").to_string()
        }
    };
    Ok(RoomMeta {
        title: d["title"].as_str().unwrap_or("").to_string(),
        // get_info 无 uname → 另取主播信息（get_anchor_in_room，失败仅留空）。
        nickname: fetch_bili_anchor_name(room),
        cover,
        room_id,
        live_status,
        live_start: if live_status == 1 {
            d["live_time"].as_i64().unwrap_or(0)
        } else {
            0
        },
    })
}

/// 拉取 B站主播昵称（get_anchor_in_room 接口）。失败返回空。
pub(super) fn fetch_bili_anchor_name(room: &str) -> String {
    let url = format!(
        "https://api.live.bilibili.com/live_user/v1/UserInfo/get_anchor_in_room?roomid={room}"
    );
    let headers = vec![
        ("User-Agent".to_string(), BROWSER_UA.to_string()),
        (
            "Referer".to_string(),
            format!("https://live.bilibili.com/{room}"),
        ),
    ];
    let Ok(r) = crate::http::request_sync("GET", &url, &headers, None) else {
        return String::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&r.body) else {
        return String::new();
    };
    v["data"]["info"]["uname"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// 开播时长文本（「H:MM:SS」）；start=0 返回空。
pub(super) fn format_duration(start: i64) -> String {
    if start <= 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(start);
    let secs = (now - start).max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    format!("{h}:{m:02}:{s:02}")
}
