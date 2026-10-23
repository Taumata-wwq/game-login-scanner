use crate::prelude::*;

use crate::AppState;

/// 启动直播会话：解析平台直播间为 http-flv 流并开始解码，
/// 阻塞至拿到首帧。返回会话信息（含 id，供 capture_live_frame 用）。
/// `platform` 为 "bilibili"（默认）或 "douyin"。多路监控：每房间一个会话。
#[tauri::command]
pub(crate) async fn live_start(
    platform: String,
    room: String,
) -> Result<crate::live::SessionInfo, String> {
    let p = crate::live::Platform::parse(&platform);
    crate::live::start(p, room).await
}

/// 停止指定直播会话；room 为空则停止全部（预览恢复屏幕画面，扫码恢复屏幕扫描）。
#[tauri::command]
pub(crate) fn live_stop(platform: Option<String>, room: Option<String>) {
    match room {
        Some(r) if !r.is_empty() => {
            let p = crate::live::Platform::parse(platform.as_deref().unwrap_or("bilibili"));
            crate::live::stop_room(p, &r);
        }
        _ => crate::live::stop_all(),
    }
}

/// 活跃直播会话列表（前端房间列表用）。
#[tauri::command]
pub(crate) fn live_list() -> Vec<crate::live::SessionInfo> {
    crate::live::list_sessions()
}

/// 设置前台（主画面）会话：恢复其拉流、暂停其余（后台零带宽零解码）。
#[tauri::command]
pub(crate) fn live_set_active(id: String) {
    if id.is_empty() {
        crate::live::pause_all();
    } else {
        crate::live::set_active(&id);
    }
}

/// 监测同步的入参（前端传已保存直播间列表）。
#[derive(serde::Deserialize)]
pub(crate) struct WatchRoom {
    platform: String,
    room: String,
}

/// 同步「开播/下播」即时监测列表（B站弹幕 WS，见 `watch.rs`；幂等）。
///
/// 弹幕 WS 在房间状态切换时实时推送 LIVE/PREPARING → `live-status` 事件，
/// 感知延迟从轮询的「≤30s」降到「≈1s」。非 B站（抖音）由轮询兜底。
#[tauri::command]
pub(crate) fn live_watch_sync(rooms: Vec<WatchRoom>, app: tauri::AppHandle) {
    let list: Vec<(String, String)> = rooms.into_iter().map(|r| (r.platform, r.room)).collect();
    crate::watch::sync(list, move |room, live_status| {
        let _ = app.emit(
            "live-status",
            serde_json::json!({
                "platform": "bilibili",
                "room": room,
                "live_status": live_status,
            }),
        );
    });
}

/// 记录直播间使用历史（前端在连接成功后调用；去重置顶并持久化）。
#[tauri::command]
pub(crate) fn live_history_push(
    room: String,
    platform: Option<String>,
    note: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let platform = platform.unwrap_or_else(|| "bilibili".to_string());
    let note = note.unwrap_or_default();
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.push_live_history(&room, &platform, &note);
    cfg.save().map_err(|e| format!("保存配置失败: {e}"))
}

/// 删除指定直播间的历史条目。
#[tauri::command]
pub(crate) fn live_history_remove(
    room: String,
    platform: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let platform = platform.unwrap_or_else(|| "bilibili".to_string());
    let platform = if platform == "douyin" {
        "douyin"
    } else {
        "bilibili"
    };
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.remove_live_history(&room, platform);
    cfg.save().map_err(|e| format!("保存配置失败: {e}"))
}

/// 设置某直播间的备注。
#[tauri::command]
pub(crate) fn live_history_set_note(
    room: String,
    platform: String,
    note: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let platform = if platform == "douyin" {
        "douyin"
    } else {
        "bilibili"
    };
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.set_live_note(&room, platform, &note);
    cfg.save().map_err(|e| format!("保存配置失败: {e}"))
}

/// 按房间顺序重排已保存直播间（拖拽排序持久化）。
#[tauri::command]
pub(crate) fn live_history_reorder(
    order: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.reorder_live_history(&order);
    cfg.save().map_err(|e| format!("保存配置失败: {e}"))
}

/// 刷新单个直播间的元数据并持久化缓存；返回最新元数据。
///
/// 失败时**返回 Err**（而非空默认值）：前端据此区分「正在获取」与「房间不存在」，
/// 若在这里吞掉错误，坏房间永远只会渲染成空标题，用户无从判断自己填错了房间号。
/// 抖音侧无严格元数据链路（`fetch_meta_strict` 对其直接转发宽松版），故其失败
/// 仍视为瞬时问题——前端只在错误文本命中「未找到该房间 / 房间不存在」时才判为
/// 终态，其余按普通失败处理。
#[tauri::command]
pub(crate) async fn live_refresh_meta(
    platform: String,
    room: String,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let plat = crate::live::Platform::parse(&platform);
    // 同步 HTTP 放到阻塞线程池，并用超时兜底：是「std::thread + mpsc::recv」
    // 无超时阻塞，单次刷新可占用一个 tokio worker 10-30 秒，几个房间并行
    // 就能把 worker 池占满，导致登录/扫码/预览等所有异步命令一起卡住。
    let room_for_fetch = room.clone();
    let meta = match tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::task::spawn_blocking(move || crate::live::fetch_meta_strict(plat, &room_for_fetch)),
    )
    .await
    {
        Ok(Ok(m)) => m.map_err(|e| format!("直播间 {room} 元数据获取失败：{e}"))?,
        Ok(Err(e)) => return Err(format!("元数据获取任务失败：{e}")),
        Err(_) => return Err(format!("直播间 {room} 元数据获取超时，请重试")),
    };
    let plat_key = plat.key();
    {
        let mut cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cfg.update_live_meta(
            &room,
            plat_key,
            &meta.title,
            &meta.nickname,
            &meta.cover,
            &meta.room_id,
            meta.live_status,
            meta.live_start,
        );
        if let Err(e) = cfg.save() {
            log::warn!("保存配置失败: {e}");
        }
    }
    Ok(serde_json::json!({
        "room": room,
        "platform": plat_key,
        "title": meta.title,
        "nickname": meta.nickname,
        "cover": meta.cover,
        "room_id": meta.room_id,
        "live_status": meta.live_status,
        "duration": crate::live::duration_text(meta.live_start),
    }))
}

/// 直播预览帧：按前端视口裁剪最新解码帧并降采样，
/// 24 字节头（w,h,mon.x,mon.y,mon.w=流宽,mon.h=流高）+ RGBA；暂无帧返回 24 字节零头。
///
/// **必须 async**：Tauri 的**同步**命令在 IPC 消息线程（Windows 上即主线程/WebView2
/// 消息循环）内联执行——本命令每帧要做 YUV→RGBA 裁剪转换（数毫秒），同步实现会
/// 每帧阻塞主线程 → 界面卡顿、掉帧。改为 async 后由 tokio 线程池执行
/// （内部再 spawn_blocking，避免占住异步工作线程）。
#[tauri::command]
pub(crate) async fn capture_live_frame(
    id: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    max_width: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mw = max_width.unwrap_or(384).clamp(80, 4096);
        match crate::live::crop_frame(&id, x, y, w, h, mw) {
            // 会话不存在（未启动/下播被移除）：0 字节 = 前端自动切回屏幕。
            crate::live::FrameOut::NoSession => Ok(tauri::ipc::Response::new(Vec::new())),
            // 欠载或帧未变：24 字节零头 = 沿用上一帧。
            crate::live::FrameOut::Empty => Ok(tauri::ipc::Response::new(vec![0u8; 24])),
            crate::live::FrameOut::Frame(payload) => Ok(tauri::ipc::Response::new(payload)),
        }
    })
    .await
    .map_err(|e| format!("直播取帧任务失败: {e}"))?
}
