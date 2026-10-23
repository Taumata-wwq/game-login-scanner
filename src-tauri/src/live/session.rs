use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::bili::{resolve_stream, Platform};
use super::decode::{decode_loop, FailFlag};
use super::frames::*;
use super::meta::{fetch_bili_meta, format_duration, RoomMeta, SessionInfo};

/// 单个直播会话（多路监控：每房间一个，独立解码线程 + 帧总线 + 预览/扫码状态）。
pub(super) struct LiveSession {
    /// 会话标识 `{platform_key}:{room}`。
    pub(super) id: String,
    pub(super) stop: Arc<AtomicBool>,
    /// 后台暂停标志：true 时解码线程**停止读 socket**（TCP 背压 → CDN 停发
    /// → 零带宽零解码），切换回前台时恢复（无需重连，秒切）。这是多路监控
    /// 「后台不占资源」的核心机制。
    pub(super) paused: Arc<AtomicBool>,
    pub(super) frames: Arc<Mutex<Frames>>,
    pub(super) room: String,
    pub(super) platform: Platform,
    /// 最近一次预览请求的视口（流坐标系）；扫码只分析该区域（与屏幕扫码
    /// 「可视视口即扫描范围」语义一致）。按会话分桶，多路互不干扰。
    pub(super) last_view: Mutex<Option<(i32, i32, i32, i32)>>,
    /// 上次发送的预览帧标识（按会话分桶）。
    pub(super) last_sent: Mutex<Option<SentKey>>,
    /// 直播间元数据（标题/主播/封面等）。
    pub(super) meta: Mutex<RoomMeta>,
}

/// 当前前台（主画面）会话 id。后台会话暂停拉流；切换时更新。
/// 扫码只扫该会话（避免后台画面误触确认）。
pub(super) static ACTIVE_ID: Mutex<Option<String>> = Mutex::new(None);

/// 扫码保活集合：这些会话即使不是主画面也**不暂停**（多房间并行扫码用）。
/// 需要运行的会话 = 主画面 ∪ 本集合；其余暂停（TCP 背压 → 零带宽零解码）。
pub(super) static SCAN_KEEPALIVE: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

pub(super) fn scan_keepalive() -> &'static Mutex<std::collections::HashSet<String>> {
    SCAN_KEEPALIVE.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

pub(super) fn set_active_id(id: Option<String>) {
    *ACTIVE_ID.lock().unwrap_or_else(PoisonError::into_inner) = id;
}

/// 当前前台会话 id（扫码用）。
pub fn active_id() -> Option<String> {
    ACTIVE_ID
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// 按「主画面 ∪ 扫码保活」重算所有会话的暂停位。从暂停恢复的会话清空显示
/// 队列与发送去重（丢弃暂停期间积压的旧帧，切换即显示最新画面）。
pub(super) fn refresh_paused() {
    let active = active_id();
    let keep: std::collections::HashSet<String> = scan_keepalive()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let sessions = lock_sessions();
    if let Some(map) = sessions.as_ref() {
        for (k, s) in map {
            let run = active.as_deref() == Some(k.as_str()) || keep.contains(k);
            let was_paused = s.paused.swap(!run, Ordering::SeqCst);
            if was_paused && run {
                let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
                f.queue.clear();
                f.scan_cache = None;
            }
        }
    }
}

/// 扫码保活开关（多房间并行扫码时由命令层调用；停止扫码后恢复普通暂停语义）。
pub fn set_scan_keepalive(id: &str, on: bool) {
    {
        let mut keep = scan_keepalive()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if on {
            keep.insert(id.to_string());
        } else {
            keep.remove(id);
        }
    }
    refresh_paused();
}

/// 设置前台会话：恢复该会话拉流，暂停其余（扫码保活者除外）。id 为空则恢复桌面模式。
pub fn set_active(id: &str) {
    set_active_id(Some(id.to_string()));
    refresh_paused();
}

/// 取消前台（如退出直播模式）：主画面为空；扫码保活会话继续运行。
pub fn pause_all() {
    set_active_id(None);
    refresh_paused();
}

/// 同时监控的直播会话上限。后台会话暂停拉流（零带宽零解码），故上限放宽。
pub(super) const MAX_SESSIONS: usize = 30;

pub(super) type SessionMap = std::collections::HashMap<String, Arc<LiveSession>>;

/// 会话池：key = `{platform_key}:{room}`。持锁仅做增删查，消费在锁外
/// （clone Arc<LiveSession> 后操作，避免阻塞其它会话的解码线程）。
pub(super) static LIVE_SESSIONS: Mutex<Option<SessionMap>> = Mutex::new(None);

pub(super) fn lock_sessions() -> std::sync::MutexGuard<'static, Option<SessionMap>> {
    LIVE_SESSIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 会话 key（平台 + 房间号；同一房间号在不同平台是不同会话）。
pub(super) fn session_key(platform: Platform, room: &str) -> String {
    format!("{}:{room}", platform.key())
}

pub(super) fn get_session(key: &str) -> Option<Arc<LiveSession>> {
    lock_sessions().as_ref()?.get(key).cloned()
}

pub(super) fn session_count() -> usize {
    lock_sessions().as_ref().map_or(0, |m| m.len())
}

pub(super) fn insert_session(key: String, s: Arc<LiveSession>) {
    lock_sessions()
        .get_or_insert_with(SessionMap::new)
        .insert(key, s);
}

pub(super) fn remove_session(key: &str) -> Option<Arc<LiveSession>> {
    lock_sessions().as_mut()?.remove(key)
}

pub(super) fn take_all_sessions() -> Vec<Arc<LiveSession>> {
    lock_sessions()
        .take()
        .map(|m| m.into_values().collect())
        .unwrap_or_default()
}

/// 启动直播会话（解析流地址 + 启动解码线程，阻塞至拿到首帧或超时）。
/// 多路监控：同一房间（同平台）重复调用即**重连**——旧会话保留在池里当占位，
/// 直到新会话拿到首帧才被替换；超过 MAX_SESSIONS 报错。
///
/// 重连准备：停掉同房间的旧会话，但**把它留在会话池里当占位**，返回该占位句柄。
///
/// 为什么不能在这里 remove_session（「刷新就丢掉当前直播间」的根因）：新会话要等首帧才
/// insert_session（通常 1~4 秒，超时 15 秒），这段窗口里 get_session 返回 None →
/// crop_frame 回 NoSession → capture_live_frame 回 0 字节，而前端协议里「0 字节 = 会话已
/// 结束」→ closeLive() 把当前直播间从列表删掉、焦点跳到别的房间或桌面。刷新于是变成
/// 「退出当前直播间」。留着占位：队列里剩的旧帧照常播出，榨干后 crop_frame 回 Empty
/// （24 字节）→ 前端沿用上一帧，正是刷新期间想要的观感。
pub(super) fn stop_old_for_reconnect(key: &str) -> Option<Arc<LiveSession>> {
    let old = get_session(key);
    if let Some(o) = &old {
        o.stop.store(true, Ordering::SeqCst);
    }
    old
}

/// 重连失败（解析失败/线程启动失败/超时/取消）的统一出口：清掉占位会话后返回错误。
///
/// 占位必须清掉，否则会留下一个永远不出帧的僵尸会话（前端一直停在上次画面）；
/// 但只在**池里仍是这个占位**时才删，避免误删并发新建的会话。
fn reconnect_failed(
    key: &str,
    placeholder: &Option<Arc<LiveSession>>,
    msg: String,
) -> Result<SessionInfo, String> {
    if let Some(o) = placeholder {
        let mut guard = lock_sessions();
        if let Some(map) = guard.as_mut() {
            if map.get(key).is_some_and(|cur| Arc::ptr_eq(cur, o)) {
                map.remove(key);
            }
        }
    }
    Err(msg)
}

pub async fn start(platform: Platform, room: String) -> Result<SessionInfo, String> {
    let key = session_key(platform, &room);
    // 该会话此前是否就是「当前前台会话」：重连要沿用这个身份，而不是无条件抢占前台
    // （否则刷新一个非前台直播间会把它切成前台，正在看的那一路被暂停）。
    let was_active = active_id().as_deref() == Some(key.as_str());
    // 同房间已存在 → 重连：停旧解码线程，但保留占位（见 stop_old_for_reconnect）。
    let old = stop_old_for_reconnect(&key);
    // 重连同一房间不新增会话，占位不占上限名额。
    if old.is_none() && session_count() >= MAX_SESSIONS {
        return Err(format!("同时监控的直播间已达上限（{MAX_SESSIONS} 个）"));
    }
    // 初始校验：房间可解析、可开播（失败快速反馈给前端）；
    // 实际拉流地址由解码线程每次连接/重连时重新获取（新 expires，自愈临时故障）。
    match platform {
        Platform::Bilibili => {
            if let Err(e) = resolve_stream(&room, 10000).await {
                return reconnect_failed(&key, &old, e);
            }
        }
        Platform::Douyin => {
            // 仅做一次解析校验（能否拿到流）；实际拉流由解码线程每次重连时重新解析。
            if let Err(e) = crate::douyin::resolve(&room).await {
                return reconnect_failed(&key, &old, e);
            }
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(Mutex::new(Frames {
        queue: std::collections::VecDeque::new(),
        latest: None,
        scan_cache: None,
        next_seq: 0,
    }));
    let fail: FailFlag = Arc::new(Mutex::new(None));
    let paused = Arc::new(AtomicBool::new(false));
    let (room2, stop2, frames2, fail2, paused2) = (
        room.clone(),
        stop.clone(),
        frames.clone(),
        fail.clone(),
        paused.clone(),
    );
    if let Err(e) = std::thread::Builder::new()
        .name(format!("live-{room}"))
        .spawn(move || decode_loop(platform, room2, stop2, frames2, fail2, paused2))
    {
        return reconnect_failed(&key, &old, format!("解码线程启动失败: {e}"));
    }
    // 等首帧（流建立 + 关键帧到达，通常 1~4 秒）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        {
            let f = frames.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(back) = f.queue.back() {
                if back.w > 0 {
                    let (w, h) = (back.w, back.h);
                    drop(f);
                    let id = session_key(platform, &room);
                    // 元数据（B站 get_info；抖音暂空）。
                    let meta = if platform == Platform::Bilibili {
                        fetch_bili_meta(&room)
                    } else {
                        RoomMeta::default()
                    };
                    insert_session(
                        id.clone(),
                        Arc::new(LiveSession {
                            id: id.clone(),
                            stop,
                            paused,
                            frames,
                            room: room.clone(),
                            platform,
                            last_view: Mutex::new(None),
                            last_sent: Mutex::new(None),
                            meta: Mutex::new(meta.clone()),
                        }),
                    );
                    // 前台归属：重连**原本就在前台**的那一路 → 保持（不动 ACTIVE_ID）；
                    // 全新会话 → 置为前台（同时只前台 1 路，扫码保活者不受影响）。
                    // 若重连的是非前台会话，绝不能 set_active——那会把用户正在看的画面
                    // 暂停掉，「刷新当前直播间」于是变成「跳到另一个直播间」。
                    if was_active {
                        refresh_paused();
                    } else {
                        set_active(&id);
                    }
                    return Ok(SessionInfo {
                        id,
                        room: room.clone(),
                        platform: platform.key().to_string(),
                        w,
                        h,
                        title: meta.title,
                        nickname: meta.nickname,
                        cover: meta.cover,
                        room_id: meta.room_id,
                        duration: format_duration(meta.live_start),
                    });
                }
            }
        }
        if stop.load(Ordering::SeqCst) {
            return reconnect_failed(&key, &old, "直播会话已被取消".to_string());
        }
        // 解码线程回报致命失败：立即返回明确错误，不必干等 15 秒超时。
        if let Some(reason) = fail.lock().unwrap_or_else(PoisonError::into_inner).take() {
            stop.store(true, Ordering::SeqCst);
            return reconnect_failed(&key, &old, reason);
        }
        if tokio::time::Instant::now() >= deadline {
            stop.store(true, Ordering::SeqCst);
            return reconnect_failed(
                &key,
                &old,
                format!(
                    "{}直播间 {room} 连接超时（15 秒内未取到画面）",
                    platform.label()
                ),
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 停止指定房间的直播会话；room 为空则停止全部。
pub fn stop_room(platform: Platform, room: &str) {
    let key = session_key(platform, room);
    if let Some(s) = remove_session(&key) {
        s.stop.store(true, Ordering::SeqCst);
        log::info!("直播会话已断开（{} {}）", s.platform.label(), s.room);
    }
    // 会话已销毁：清掉扫码保活。否则该 id 会永远留在集合里，之后同房间重建时
    // refresh_paused 会认为「不该暂停」，后台白拉流（keepalive 本意是只在扫码期间保活）。
    set_scan_keepalive(&key, false);
    // 被停的是前台会话：前台清空（无自动接管，由前端切换）。
    if active_id().as_deref() == Some(key.as_str()) {
        set_active_id(None);
    }
}

/// 停止全部直播会话。
pub fn stop_all() {
    set_active_id(None);
    for s in take_all_sessions() {
        s.stop.store(true, Ordering::SeqCst);
        // 同 stop_room：会话销毁时一并清掉扫码保活，避免残留 id 影响后续重建。
        set_scan_keepalive(&s.id, false);
        log::info!("直播会话已断开（{} {}）", s.platform.label(), s.room);
    }
}

/// 活跃会话列表（前端房间列表用）。
pub fn list_sessions() -> Vec<SessionInfo> {
    let sessions = lock_sessions();
    let Some(map) = sessions.as_ref() else {
        return Vec::new();
    };
    map.values()
        .map(|s| {
            let (w, h) = {
                let f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
                f.latest.as_ref().map_or((0, 0), |b| (b.w, b.h))
            };
            let m = s
                .meta
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            SessionInfo {
                id: s.id.clone(),
                room: s.room.clone(),
                platform: s.platform.key().to_string(),
                w,
                h,
                title: m.title,
                nickname: m.nickname,
                cover: m.cover,
                room_id: m.room_id,
                duration: format_duration(m.live_start),
            }
        })
        .collect()
}

/// 是否存在任意活跃会话。
pub fn active() -> bool {
    session_count() > 0
}

/// 诊断指定会话：最新解码帧的 (seq, 媒体时间戳, 队列深度)。
pub fn diag(id: &str) -> Option<(u64, u32, usize)> {
    let s = get_session(id)?;
    let f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
    let b = f.latest.as_ref()?;
    Some((b.seq, b.ts, f.queue.len()))
}
