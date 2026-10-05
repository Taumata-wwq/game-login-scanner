//! 崩坏3 扫码器库入口：模块声明 + Tauri 命令 + 运行时装配。

pub mod config;
pub mod constants;
pub mod crypto;
pub mod duplication;
pub mod http;
pub mod qr;
pub mod scan;
pub mod screenshot;
pub mod sdk;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};
use uuid::Uuid;

use config::{AppConfig, Channel, Game};
use sdk::bili::BiliQrSession;
use sdk::official::OfficialQrSession;
/// 双渠道登录会话槽。
#[derive(Default)]
struct QrLoginSlots {
    bili: Option<BiliQrSession>,
    official: Option<OfficialQrSession>,
}

/// 共享运行时状态。
pub struct AppState {
    pub stop: Arc<AtomicBool>,
    pub scanning: Arc<AtomicBool>,
    pub config: Mutex<AppConfig>,
    login_qrs: Mutex<QrLoginSlots>,
    /// 登录二维码申请的代际号：防止并发 `login_qr_start`（用户快速切换渠道）时
    /// 先发起但后返回的旧请求覆盖新会话。
    login_qr_seq: Mutex<u64>,
    /// 用户指定的扫描区域（虚拟桌面绝对坐标，会话内有效，不持久化）。
    pub region: Mutex<Option<(i32, i32, i32, i32)>>,
    /// 最近扫码成功的时刻（秒），用于 2 分钟内最多 10 次的频率限制。
    scan_success_times: Mutex<Vec<i64>>,
    /// 独立人机验证窗口返回的结果（aigis 字符串）。
    captcha_result: Mutex<Option<String>>,
}

fn now_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 扫码成功频率限制：2 分钟内最多 10 次。超限返回错误信息。
pub fn check_scan_throttle(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let now = now_sec();
    let mut times = state
        .scan_success_times
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    times.retain(|t| now - *t < 120);
    if times.len() >= 10 {
        return Err("2 分钟内扫码成功次数已达上限（10 次），请稍后再试".to_string());
    }
    times.push(now);
    Ok(())
}

// ---------- 配置 ----------

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> AppConfig {
    state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[tauri::command]
fn save_config(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    cfg: AppConfig,
) -> Result<(), String> {
    let mut c = cfg;
    c.sanitize();
    c.save().map_err(|e| e.to_string())?;
    let prev_on_top = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .always_on_top;
    if prev_on_top != c.always_on_top {
        apply_always_on_top(&app, c.always_on_top);
    }
    *state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = c;
    Ok(())
}

/// 前端日志落盘：窗口操作失败等前端异常写入 bh3-scanner.log，便于远程排查。
#[tauri::command]
fn frontend_log(level: String, msg: String) {
    match level.as_str() {
        "error" => log::error!("[web] {msg}"),
        "warn" => log::warn!("[web] {msg}"),
        _ => log::info!("[web] {msg}"),
    }
}

/// 应用窗口置顶设置。
fn apply_always_on_top(app: &tauri::AppHandle, on_top: bool) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_always_on_top(on_top);
    }
}

/// 记住主窗口当前坐标（供关闭 / 自动退出两条退出路径共用）。
pub(crate) fn save_window_position(app: &tauri::AppHandle) {
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    let Ok(pos) = w.outer_position() else {
        return;
    };
    let state = app.state::<AppState>();
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.win_x = Some(pos.x);
    cfg.win_y = Some(pos.y);
    let _ = cfg.save();
}

/// 回填官服账号昵称：对 uname 为空的官服账号逐个查询米游社并保存。
/// 供前端启动时调用一次（旧账号在昵称功能上线前登录，uname 为空）。
#[tauri::command]
async fn refresh_official_unames(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // 收集待回填的 (uid, stoken, mid)。
    let pending: Vec<(String, String, String)> = {
        let cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cfg.official_accounts
            .iter()
            .filter(|a| a.uname.is_empty())
            .map(|a| (a.uid.clone(), a.stoken.clone(), a.mid.clone()))
            .collect()
    };
    if pending.is_empty() {
        return Ok(());
    }
    let mut changed = false;
    for (uid, stoken, mid) in pending {
        if let Ok(name) = sdk::official::get_uname(&uid, &stoken, &mid).await {
            if !name.is_empty() {
                let mut cfg = state
                    .config
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(a) = cfg.official_accounts.iter_mut().find(|a| a.uid == uid) {
                    a.uname = name;
                    changed = true;
                }
            }
        }
    }
    if changed {
        let cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = cfg.save();
        let _ = app.emit("accounts-updated", ());
    }
    Ok(())
}

// ---------- 账号管理 ----------

/// 选择要使用的账号（全局唯一）。
/// 游戏登录码是通用的——任何渠道账号都能确认登录，因此这里把「当前账号」
/// 明确设为该账号：扫描时**只用它**确认，不做多渠道轮询。
#[tauri::command]
fn select_account(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    game: String,
    channel: String,
    uid: String,
) -> Result<(), String> {
    let channel = Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !cfg.set_current(&game, channel, &uid) {
        return Err("账号不存在".to_string());
    }
    let _ = cfg.save();
    drop(cfg);
    // 切换到 B 服账号时立即后台预校验（扫描确认零等待；失败提前暴露）。
    if channel == Channel::Bilibili {
        prewarm_bili_account(&app);
    }
    Ok(())
}

/// 删除某渠道的一个账号记录。
#[tauri::command]
fn delete_account(
    state: tauri::State<'_, AppState>,
    game: String,
    channel: String,
    uid: String,
) -> Result<(), String> {
    let channel = Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match channel {
        Channel::Bilibili => cfg.remove_bili(&game, &uid),
        Channel::Official => cfg.remove_official(&game, &uid),
    }
    let _ = cfg.save();
    Ok(())
}

/// 设置账号备注名 + 备注信息（双击账号行编辑）。
#[tauri::command]
fn set_account_meta(
    state: tauri::State<'_, AppState>,
    game: String,
    channel: String,
    uid: String,
    alias: String,
    remark: String,
) -> Result<(), String> {
    let channel = Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let ok = match channel {
        Channel::Bilibili => cfg
            .bili_accounts
            .iter_mut()
            .find(|t| t.uid == uid && t.game == game)
            .map(|t| {
                t.alias = alias;
                t.remark = remark;
            }),
        Channel::Official => cfg
            .official_accounts
            .iter_mut()
            .find(|t| t.uid == uid && t.game == game)
            .map(|t| {
                t.alias = alias;
                t.remark = remark;
            }),
    };
    if ok.is_none() {
        return Err("账号不存在".to_string());
    }
    let _ = cfg.save();
    Ok(())
}

/// 按给定 uid 顺序重排某渠道账号列表（前端拖拽排序后调用）。
#[tauri::command]
fn reorder_accounts(
    state: tauri::State<'_, AppState>,
    game: String,
    channel: String,
    order: Vec<String>,
) -> Result<(), String> {
    let channel = Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // 仅重排当前游戏的账号：给该游戏的账号按 order 排序，其它游戏保持原相对顺序。
    let idx = |uid: &str| order.iter().position(|u| u == uid).unwrap_or(usize::MAX);
    match channel {
        Channel::Bilibili => {
            cfg.bili_accounts.sort_by_key(|t| {
                if t.game == game { idx(&t.uid) } else { usize::MAX }
            });
        }
        Channel::Official => {
            cfg.official_accounts.sort_by_key(|t| {
                if t.game == game { idx(&t.uid) } else { usize::MAX }
            });
        }
    }
    let _ = cfg.save();
    Ok(())
}

/// 切换游戏：保存当前游戏渠道，恢复目标游戏渠道与账号。
#[tauri::command]
fn switch_game(state: tauri::State<'_, AppState>, game: String) -> Result<(), String> {
    let g = Game::parse(&game).ok_or_else(|| "未知游戏".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cfg.current_game == g {
        return Ok(());
    }
    // 各游戏的渠道/账号本就按 game 存储，切换只需改 current_game。
    cfg.current_game = g;
    let _ = cfg.save();
    Ok(())
}

/// 游戏 → 存储键（用于 game_channel 映射）。
fn game_key(g: Game) -> String {
    match g {
        Game::Bh3 => "bh3",
        Game::Hsr => "hsr",
        Game::Genshin => "genshin",
        Game::Zzz => "zzz",
        Game::Endfield => "endfield",
        Game::Arknights => "arknights",
    }
    .to_string()
}

// ---------- 鹰角账号管理 ----------

/// 选择鹰角账号。
#[tauri::command]
fn hypergryph_select(
    state: tauri::State<'_, AppState>,
    game: String,
    phone: String,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !cfg
        .hypergryph_accounts
        .iter()
        .any(|t| t.phone == phone && t.game == game)
    {
        return Err("账号不存在".to_string());
    }
    cfg.active_hypergryph.insert(game, phone);
    let _ = cfg.save();
    Ok(())
}

/// 删除鹰角账号。
#[tauri::command]
fn hypergryph_delete(
    state: tauri::State<'_, AppState>,
    game: String,
    phone: String,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.remove_hypergryph(&game, &phone);
    let _ = cfg.save();
    Ok(())
}

/// 设置鹰角账号备注。
#[tauri::command]
fn hypergryph_set_meta(
    state: tauri::State<'_, AppState>,
    game: String,
    phone: String,
    alias: String,
    remark: String,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let found = cfg
        .hypergryph_accounts
        .iter_mut()
        .find(|t| t.phone == phone && t.game == game)
        .map(|t| {
            t.alias = alias;
            t.remark = remark;
        });
    if found.is_none() {
        return Err("账号不存在".to_string());
    }
    let _ = cfg.save();
    Ok(())
}

// ---------- 渠道账号登录（米哈游 / B站：验证码 / 密码） ----------

/// 读取配置中的 device_id（空则回退默认），供登录接口设备绑定。
fn cfg_device_id(state: &tauri::State<'_, AppState>) -> String {
    let id = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .device_id
        .clone();
    if id.is_empty() {
        constants::DSPROD_DEVICE_FP_DEFAULT.to_string()
    } else {
        id
    }
}

/// 获取真实设备指纹（优先用配置缓存；无则请求 getFp 并持久化）。
/// 这是米哈游登录风控的核心——假指纹会触发「请求频繁」(-1001)。
async fn ensure_device_fp(state: &tauri::State<'_, AppState>) -> Result<String, String> {
    let (device_id, cached, seed_id, seed_time) = {
        let cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            cfg.device_id.clone(),
            cfg.device_fp.clone(),
            cfg.fp_seed_id.clone(),
            cfg.fp_seed_time.clone(),
        )
    };
    if !cached.is_empty() {
        return Ok(cached);
    }
    let device_id = if device_id.is_empty() {
        constants::DSPROD_DEVICE_FP_DEFAULT.to_string()
    } else {
        device_id
    };
    // 生成 seed（UUID + 时间戳）。
    let (seed_id, seed_time) = if seed_id.is_empty() {
        (
            uuid::Uuid::new_v4().to_string(),
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0))
            .to_string(),
        )
    } else {
        (seed_id, seed_time)
    };
    let fp = sdk::cred_login::fetch_device_fp(&device_id, &seed_id, &seed_time).await?;
    {
        let mut cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cfg.device_fp = fp.clone();
        cfg.fp_seed_id = seed_id;
        cfg.fp_seed_time = seed_time;
        let _ = cfg.save();
    }
    Ok(fp)
}

/// 发送短信验证码（channel: official | bilibili）。
#[tauri::command]
async fn cred_send_code(
    state: tauri::State<'_, AppState>,
    channel: String,
    game: String,
    phone: String,
    aigis: Option<String>,
) -> Result<serde_json::Value, String> {
    let _ = game;
    let aigis = aigis.unwrap_or_default();
    match channel.as_str() {
        "official" => {
            let device_id = cfg_device_id(&state);
            let device_fp = ensure_device_fp(&state).await?;
            match sdk::cred_login::mihoyo_send_sms(&device_id, &device_fp, &phone, &aigis).await? {
                Some(a) => Ok(serde_json::json!({ "status": "captcha", "aigis": a })),
                None => Ok(serde_json::json!({ "status": "ok" })),
            }
        }
        "bilibili" => {
            sdk::cred_login::bili_send_sms(&phone).await?;
            Ok(serde_json::json!({ "status": "ok" }))
        }
        _ => Err("未知渠道".to_string()),
    }
}

/// 验证码登录。
#[tauri::command]
async fn cred_login_sms(
    state: tauri::State<'_, AppState>,
    channel: String,
    game: String,
    phone: String,
    code: String,
    aigis: Option<String>,
) -> Result<serde_json::Value, String> {
    let aigis = aigis.unwrap_or_default();
    let outcome = match channel.as_str() {
        "official" => {
            let device_id = cfg_device_id(&state);
            let device_fp = ensure_device_fp(&state).await?;
            sdk::cred_login::mihoyo_login_sms(&device_id, &device_fp, &phone, &code, &aigis).await?
        }
        "bilibili" => sdk::cred_login::bili_login_sms(&phone, &code, &aigis).await?,
        _ => return Err("未知渠道".to_string()),
    };
    apply_cred_outcome(&state, &game, outcome).await
}

/// 密码登录。
#[tauri::command]
async fn cred_login_pwd(
    state: tauri::State<'_, AppState>,
    channel: String,
    game: String,
    account: String,
    password: String,
    aigis: Option<String>,
) -> Result<serde_json::Value, String> {
    let aigis = aigis.unwrap_or_default();
    let outcome = match channel.as_str() {
        "official" => {
            let device_id = cfg_device_id(&state);
            let device_fp = ensure_device_fp(&state).await?;
            sdk::cred_login::mihoyo_login_pwd(&device_id, &device_fp, &account, &password, &aigis).await?
        }
        "bilibili" => sdk::cred_login::bili_login_pwd(&account, &password, &aigis).await?,
        _ => return Err("未知渠道".to_string()),
    };
    apply_cred_outcome(&state, &game, outcome).await
}

/// 把登录结果写入配置（成功）或转成前端可渲染的 captcha 响应。
async fn apply_cred_outcome(
    state: &tauri::State<'_, AppState>,
    game: &str,
    outcome: sdk::cred_login::CredOutcome,
) -> Result<serde_json::Value, String> {
    use sdk::cred_login::CredOutcome;
    match outcome {
        CredOutcome::NeedCaptcha(data) => {
            Ok(serde_json::json!({ "status": "captcha", "aigis": data }))
        }
        CredOutcome::MiyousheOk { uid, stoken, mid } => {
            // 查询米游社昵称（失败不影响登录，回退 uid 显示）。
            let uname = sdk::official::get_uname(&uid, &stoken, &mid)
                .await
                .unwrap_or_default();
            let mut cfg = state
                .config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cfg.upsert_official(config::OfficialToken {
                uid,
                stoken,
                mid,
                uname,
                game: game.to_string(),
                alias: String::new(),
                remark: String::new(),
                last_login_succ: true,
                last_used_at: 0,
            });
            let _ = cfg.save();
            Ok(serde_json::json!({ "status": "ok" }))
        }
        CredOutcome::BiliOk { uid, access_key } => {
            let mut cfg = state
                .config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cfg.upsert_bili(config::BiliToken {
                uid,
                access_key,
                uname: String::new(),
                game: game.to_string(),
                alias: String::new(),
                remark: String::new(),
                last_login_succ: true,
                last_used_at: 0,
            });
            let _ = cfg.save();
            Ok(serde_json::json!({ "status": "ok" }))
        }
    }
}

// ---------- 鹰角账号登录（短信验证码） ----------

/// 发送鹰角登录短信验证码。
#[tauri::command]
async fn hypergryph_send_code(phone: String) -> Result<(), String> {
    sdk::hypergryph::send_phone_code(&phone).await
}

/// 鹰角短信验证码登录：成功则保存账号并设为激活。
#[tauri::command]
async fn hypergryph_login(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    phone: String,
    code: String,
) -> Result<(), String> {
    let token = sdk::hypergryph::token_by_phone_code(&phone, &code).await?;
    {
        let mut cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let game = game_key(cfg.current_game);
        cfg.upsert_hypergryph(sdk::hypergryph::HypergryphToken {
            phone: phone.clone(),
            token,
            game,
            alias: String::new(),
            remark: String::new(),
            last_login_succ: true,
            last_used_at: sdk::hypergryph::touch_now(),
        });
        let _ = cfg.save();
    }
    let _ = app.emit("accounts-updated", ());
    Ok(())
}

// ---------- 扫码登录（账号登录，双渠道同一张界面同时等待） ----------

/// 为 B 服与官服**同时**生成登录二维码：B 服码用 B 站 App 扫、官服码用米游社
/// App 扫，哪个先确认就用哪个登录，无需切换渠道。
/// 返回 (bili_svg, official_svg)。
/// 生成指定渠道的登录二维码（主界面选中哪个渠道，就生成哪个）。
/// 返回 (渠道名, 二维码 SVG)。
#[tauri::command]
async fn login_qr_start(
    state: tauri::State<'_, AppState>,
    channel: String,
) -> Result<(String, String), String> {
    let ch = config::Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    // 代际号：并发 start 时只有最新一次的结果会写入会话。
    let seq = {
        let mut s = state
            .login_qr_seq
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *s += 1;
        *s
    };

    let device_id = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .device_id
        .clone();

    // 仅申请所选渠道。
    let mut slots = QrLoginSlots::default();
    let svg = match ch {
        config::Channel::Bilibili => {
            let bili = sdk::bili::qr_login_start().await?;
            let svg = qr::qr_svg(&bili.qr_url)?;
            slots.bili = Some(bili);
            svg
        }
        config::Channel::Official => {
            let official = sdk::official::qr_login_start(
                &device_id,
                constants::DSPROD_DEVICE_FP_DEFAULT,
            )
            .await?;
            let svg = qr::qr_svg(&official.qr_url)?;
            slots.official = Some(official);
            svg
        }
    };

    // 仍是最新请求才写入；过期请求的结果静默丢弃。
    let is_latest = {
        let s = state
            .login_qr_seq
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let latest = *s == seq;
        if latest {
            *state
                .login_qrs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = slots;
        }
        latest
    };
    if !is_latest {
        return Err("已取消".to_string());
    }
    Ok((channel, svg))
}

/// 轮询内部统一结果。
#[derive(Clone)]
enum PollOutcome {
    Waiting,
    Scanned,
    Expired,
    ConfirmedBili {
        uid: String,
        access_key: String,
    },
    ConfirmedOfficial {
        uid: String,
        stoken: String,
        mid: String,
        uname: String,
    },
}

/// 轮询扫码登录状态（双渠道都轮询，先确认者生效）。
/// 返回 (状态, 渠道, uid, 昵称)；渠道为空表示与渠道无关。
#[tauri::command]
async fn login_qr_poll(
    state: tauri::State<'_, AppState>,
) -> Result<(String, String, String, String), String> {
    use sdk::bili::BiliQrStatus;
    use sdk::official::OfficialQrStatus;

    let device_id = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .device_id
        .clone();

    // 取出两个会话快照（Option 克隆；会话本体留在槽内由下方写回）。
    let (bili_snap, official_snap) = {
        let slots = state
            .login_qrs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (slots.bili.clone(), slots.official.clone())
    };
    if bili_snap.is_none() && official_snap.is_none() {
        return Err("没有进行中的扫码登录".to_string());
    }

    // 网络错误不吞会话：轮询失败时槽内会话原样保留，前端重试即可。
    let bili_poll = async {
        match &bili_snap {
            None => Ok(PollOutcome::Waiting),
            Some(s) => sdk::bili::qr_login_poll(&s.auth_code)
                .await
                .map(|st| match st {
                    BiliQrStatus::Waiting => PollOutcome::Waiting,
                    BiliQrStatus::Scanned => PollOutcome::Scanned,
                    BiliQrStatus::Expired => PollOutcome::Expired,
                    BiliQrStatus::Confirmed { uid, access_key } => {
                        PollOutcome::ConfirmedBili { uid, access_key }
                    }
                }),
        }
    };
    let official_poll = async {
        match &official_snap {
            None => Ok(PollOutcome::Waiting),
            Some(s) => sdk::official::qr_login_poll(
                &s.ticket,
                &device_id,
                constants::DSPROD_DEVICE_FP_DEFAULT,
            )
            .await
            .map(|st| match st {
                OfficialQrStatus::Waiting => PollOutcome::Waiting,
                OfficialQrStatus::Scanned => PollOutcome::Scanned,
                OfficialQrStatus::Expired => PollOutcome::Expired,
                OfficialQrStatus::Confirmed {
                    uid,
                    stoken,
                    mid,
                    uname,
                } => PollOutcome::ConfirmedOfficial {
                    uid,
                    stoken,
                    mid,
                    uname,
                },
            }),
        }
    };
    let (bili_out, official_out) = tokio::join!(bili_poll, official_poll);

    // 任一渠道确认即写入账号并返回；会话槽清空对应渠道。
    // 锁不跨 await：确认处理里每次短暂取锁。
    for out in [bili_out.clone(), official_out.clone()] {
        let out = match out {
            Ok(o) => o,
            Err(e) => return Err(e),
        };
        match out {
            PollOutcome::ConfirmedBili { uid, access_key } => {
                let uname = sdk::bili::get_user_info(&uid, &access_key)
                    .await
                    .unwrap_or_default();
                {
                    let mut cfg = state
                        .config
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let game = game_key(cfg.current_game);
                    cfg.upsert_bili(config::BiliToken {
                        uid: uid.clone(),
                        access_key,
                        uname: uname.clone(),
                        game,
                        alias: String::new(),
                        remark: String::new(),
                        last_login_succ: true,
                        last_used_at: 0,
                    });
                    let _ = cfg.save();
                }
                state
                    .login_qrs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .bili = None;
                return Ok(("confirmed".into(), "bilibili".into(), uid, uname));
            }
            PollOutcome::ConfirmedOfficial {
                uid,
                stoken,
                mid,
                mut uname,
            } => {
                // 扫码登录响应的 user_info 无 nickname，单独查询米游社获取昵称；
                // 失败则回退 uid 显示（不影响登录）。
                if uname.is_empty() {
                    uname = sdk::official::get_uname(&uid, &stoken, &mid)
                        .await
                        .unwrap_or_default();
                }
                {
                    let mut cfg = state
                        .config
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let game = game_key(cfg.current_game);
                    cfg.upsert_official(config::OfficialToken {
                        uid: uid.clone(),
                        stoken,
                        mid,
                        uname: uname.clone(),
                        game,
                        alias: String::new(),
                        remark: String::new(),
                        last_login_succ: true,
                        last_used_at: 0,
                    });
                    let _ = cfg.save();
                }
                state
                    .login_qrs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .official = None;
                let name = if uname.is_empty() {
                    format!("uid {uid}")
                } else {
                    uname
                };
                return Ok(("confirmed".into(), "official".into(), uid, name));
            }
            _ => {}
        }
    }
    // 未完成：聚合状态（任一已扫码 → scanned；任一过期 → expired；否则 waiting）。
    let mut status = "waiting";
    let mut scanned = false;
    let mut expired = false;
    for out in [bili_out, official_out] {
        match out {
            Ok(PollOutcome::Scanned) => scanned = true,
            Ok(PollOutcome::Expired) => expired = true,
            _ => {}
        }
    }
    if expired {
        // 双渠道码各自 180s 有效；过期后清空会话，前端自动重生成。
        status = "expired";
        *state
            .login_qrs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = QrLoginSlots::default();
    } else if scanned {
        status = "scanned";
    }
    Ok((
        status.to_string(),
        String::new(),
        String::new(),
        String::new(),
    ))
}

/// 取消进行中的扫码登录（关闭二维码界面时调用）。
#[tauri::command]
fn login_qr_cancel(state: tauri::State<'_, AppState>) {
    *state
        .login_qrs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = QrLoginSlots::default();
}

// ---------- 扫描区域 ----------

/// 预览 GDI 兜底缓存：桌面复制不可用时降级（内部限频 15fps），
/// 保存最近一次负载（前 24 字节为头部，用于「帧未变」的头部专用响应）。
struct GdiPreviewCache {
    at: std::time::Instant,
    payload: Vec<u8>,
}

static GDI_PREVIEW: Mutex<Option<GdiPreviewCache>> = Mutex::new(None);

/// 预览数据帧发送节流：桌面内容高速变化时上限为 60fps（16ms），避免前端 60Hz
/// 轮询时重复发送同一帧。区域或宽度变化（平移/缩放）时不节流，保证立刻拿到新画面。
/// 预览节流记录：（上次发送时刻， 请求区域， 输出宽度）
type PreviewThrottle = (std::time::Instant, (i32, i32, i32, i32), u32);
static LAST_PREVIEW_DATA: Mutex<Option<PreviewThrottle>> = Mutex::new(None);

/// 桌面预览：返回**当前可视视口**的实时缩略图（前端缩放/平移后只取所视区域，
/// 高倍放大时每输出像素仅覆盖少量屏幕像素 → 清晰），供前端当「监视器」展示。
///
/// 数据源优先走 DXGI 桌面复制（静止画面近零开销），不可用时回退 GDI（限频）。
///
/// 返回原始二进制（`tauri::ipc::Response`，前端收到 ArrayBuffer）：
/// 24 字节小端头 + RGBA 数据，头部布局
/// `width:u32, height:u32, mon.x:i32, mon.y:i32, mon.w:i32, mon.h:i32`。
/// **仅有 24 字节头部**表示「画面未变化」，前端直接沿用上一帧。
#[tauri::command]
async fn capture_preview(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    max_width: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    // 用带 1 秒缓存的 screen_rects() 取代每帧的 screenshots::Screen::all()
    //（后者每次枚举所有显示器，是预览帧率上不去的瓶颈之一）。
    let screens = crate::screenshot::screen_rects();
    if screens.is_empty() {
        return Err("未找到可用显示器".to_string());
    }
    // 主窗口中心点所在显示器（找不到则第一块屏）；预览始终取该屏，
    // 区域越界部分由 duplication 裁剪夹紧。
    let center = app
        .get_webview_window("main")
        .and_then(|win| {
            let pos = win.outer_position().ok()?;
            let size = win.outer_size().ok()?;
            Some((
                pos.x + size.width as i32 / 2,
                pos.y + size.height as i32 / 2,
            ))
        })
        .unwrap_or((0, 0));
    let d = screens
        .iter()
        .find(|(x, y, w, h)| {
            center.0 >= *x && center.0 < *x + *w && center.1 >= *y && center.1 < *y + *h
        })
        .copied()
        .unwrap_or(screens[0]);
    let (dx, dy, mw, mh) = d;
    let want_w = max_width.unwrap_or(384).clamp(80, 4096);
    // 注：实际输出宽度由 duplication::preview_frame 进一步钳制到「不超过裁剪区
    // 实际像素数」（禁止上采样），避免放大时出现黑色网格线。
    // 区域非法（未初始化）时回退整屏。
    let (rx, ry, rw, rh) = if w > 0 && h > 0 {
        (x, y, w, h)
    } else {
        (dx, dy, mw, mh)
    };

    match crate::duplication::preview_frame((rx, ry, rw, rh), want_w) {
        Ok(crate::duplication::PreviewOut::Frame(rgba, tw, th)) => {
            // 数据帧节流到 60fps：区域与宽度都没变、距上次发送 < 16ms 时沿用旧帧。
            let now = std::time::Instant::now();
            let mut last = LAST_PREVIEW_DATA
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let throttled = matches!(
                last.as_ref(),
                Some((t, k, tw2))
                if now.duration_since(*t).as_millis() < 16 && *k == (rx, ry, rw, rh) && *tw2 == tw
            );
            if throttled {
                return Ok(tauri::ipc::Response::new(vec![0u8; 24]));
            }
            *last = Some((now, (rx, ry, rw, rh), tw));
            Ok(tauri::ipc::Response::new(preview_payload(
                tw, th, dx, dy, mw, mh, rgba,
            )))
        }
        Ok(crate::duplication::PreviewOut::Unchanged) => {
            // 24 字节头部专用响应（全零）：前端据此跳过重绘。
            Ok(tauri::ipc::Response::new(vec![0u8; 24]))
        }
        Err(_) => {
            // GDI 兜底：限频 15fps，未到间隔时沿用缓存帧。
            let mut g = GDI_PREVIEW
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(c) = g.as_ref() {
                if c.at.elapsed() < std::time::Duration::from_millis(16) {
                    return Ok(tauri::ipc::Response::new(c.payload[..24].to_vec()));
                }
            }
            let frame = crate::screenshot::capture_region(rx, ry, rw, rh).ok_or("截取屏幕失败")?;
            let tw = frame.w.clamp(1, want_w);
            let th = ((frame.h as u64 * tw as u64) / frame.w as u64).max(1) as u32;
            let mut rgba = Vec::with_capacity(tw as usize * th as usize * 4);
            for ty in 0..th {
                let sy = (ty * frame.h) / th;
                for tx in 0..tw {
                    let sx = (tx * frame.w) / tw;
                    let i = ((sy * frame.w) + sx) as usize * 3;
                    rgba.push(frame.rgb[i]);
                    rgba.push(frame.rgb[i + 1]);
                    rgba.push(frame.rgb[i + 2]);
                    rgba.push(255);
                }
            }
            let payload = preview_payload(tw, th, dx, dy, mw, mh, rgba);
            *g = Some(GdiPreviewCache {
                at: std::time::Instant::now(),
                payload: payload.clone(),
            });
            Ok(tauri::ipc::Response::new(payload))
        }
    }
}

/// 组装预览二进制负载（24 字节头 + RGBA）。
fn preview_payload(tw: u32, th: u32, mx: i32, my: i32, mw: i32, mh: i32, rgba: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(24 + rgba.len());
    out.extend_from_slice(&tw.to_le_bytes());
    out.extend_from_slice(&th.to_le_bytes());
    out.extend_from_slice(&mx.to_le_bytes());
    out.extend_from_slice(&my.to_le_bytes());
    out.extend_from_slice(&mw.to_le_bytes());
    out.extend_from_slice(&mh.to_le_bytes());
    out.extend_from_slice(&rgba);
    out
}

/// 瓦片（拖拽底图）增量帧。开启瓦片模式后，每次返回自上次以来变化的瓦片。
/// 负载：8 字节头（tw,th）+ 每瓦片 16 字节头（col,row,w,h）+ RGBA。
#[tauri::command]
fn capture_tiles(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    max_width: u32,
) -> Result<tauri::ipc::Response, String> {
    let screens = crate::screenshot::screen_rects();
    if screens.is_empty() {
        return Err("未找到可用显示器".to_string());
    }
    let center = app
        .get_webview_window("main")
        .and_then(|win| {
            let pos = win.outer_position().ok()?;
            let size = win.outer_size().ok()?;
            Some((pos.x + size.width as i32 / 2, pos.y + size.height as i32 / 2))
        })
        .unwrap_or((0, 0));
    let d = screens
        .iter()
        .find(|(sx, sy, sw, sh)| {
            center.0 >= *sx && center.0 < *sx + *sw && center.1 >= *sy && center.1 < *sy + *sh
        })
        .copied()
        .unwrap_or(screens[0]);
    let (dx, dy, mw, mh) = d;
    let (rx, ry, rw, rh) = if w > 0 && h > 0 { (x, y, w, h) } else { (dx, dy, mw, mh) };
    let want_w = max_width.clamp(80, 4096);
    match crate::duplication::tile_frame((rx, ry, rw, rh), want_w) {
        Ok(Some((tw, th, tiles))) => {
            let mut out = Vec::with_capacity(8 + tiles.len() * 16);
            out.extend_from_slice(&tw.to_le_bytes());
            out.extend_from_slice(&th.to_le_bytes());
            for (c, r, tw2, th2, rgba) in tiles {
                out.extend_from_slice(&c.to_le_bytes());
                out.extend_from_slice(&r.to_le_bytes());
                out.extend_from_slice(&tw2.to_le_bytes());
                out.extend_from_slice(&th2.to_le_bytes());
                out.extend_from_slice(&rgba);
            }
            Ok(tauri::ipc::Response::new(out))
        }
        // 无变化：仅 8 字节头。
        Ok(None) => Ok(tauri::ipc::Response::new(vec![0u8; 8])),
        Err(_) => Err("桌面复制不可用".to_string()),
    }
}

/// 开启/关闭瓦片（拖拽底图）模式。
#[tauri::command]
fn set_tile_mode(x: i32, y: i32, w: i32, h: i32, on: bool) {
    crate::duplication::set_tile_mode((x, y, w, h), on);
}

/// 暂停/恢复预览：暂停时清除后端预览关注区，让帧总线跳过拷贝（避免暂停态 GPU 升高）。
#[tauri::command]
fn set_preview_active(active: bool) {
    if !active {
        crate::duplication::clear_preview_roi();
    }
}

/// 设置扫描区域（虚拟桌面绝对坐标，物理像素）；w/h 非正表示恢复全屏。
#[tauri::command]
fn set_scan_region(
    state: tauri::State<'_, AppState>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<(), String> {
    *state
        .region
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = if w > 0 && h > 0 {
        Some((x, y, w, h))
    } else {
        None
    };
    Ok(())
}

// ---------- 扫码 ----------

/// 后台预校验当前 B 服账号（verify 并缓存 bh_info），
/// 使「开始扫码」命中二维码后无需再等一次校验往返。
fn prewarm_bili_account(app: &tauri::AppHandle) {
    let cfg = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let game = game_key(cfg.current_game);
    if cfg.game_channel.get(&game).copied().flatten() != Some(Channel::Bilibili) {
        return;
    }
    let Some(b) = cfg.active_bili(&game) else {
        return;
    };
    let uid = b.uid.clone();
    let ak = b.access_key.clone();
    tauri::async_runtime::spawn(async move {
        match sdk::mihoyo::verify_cached(&uid, &ak).await {
            Ok(_) => log::info!("B 服账号预校验完成（{uid}）"),
            Err(e) => log::warn!("B 服账号预校验失败（{uid}）: {e}"),
        }
    });
}

/// 校验凭据并启动扫描线程（命令与「启动自动登录」共用）。
/// 完全自动：只要任一渠道有激活账号即可开始，扫描时按码自动路由。
fn try_start_scan(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    if state.scanning.swap(true, Ordering::SeqCst) {
        return Err("扫描已在进行中".to_string());
    }

    let cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let game = game_key(cfg.current_game);
    let bili = cfg.active_bili(&game).cloned().unwrap_or_default();
    let official = cfg.active_official(&game).cloned().unwrap_or_default();
    let hyper = cfg.active_hypergryph(&game).cloned().unwrap_or_default();
    // 鹰角游戏用鹰角账号；米哈游系用 game_channel 对应的账号。
    let current_channel = if cfg.current_game.is_hypergryph() {
        let ready = !hyper.phone.is_empty() && !hyper.token.is_empty();
        if !ready {
            state.scanning.store(false, Ordering::SeqCst);
            return Err("请先在「账号」中添加鹰角账号（手机号验证码登录）".to_string());
        }
        // 鹰角游戏用 Official 占位（下游只对米哈游码生效，鹰角码走独立分支）。
        Channel::Official
    } else {
        let Some(ch) = cfg.game_channel.get(&game).copied().flatten() else {
            state.scanning.store(false, Ordering::SeqCst);
            return Err("请先在「账号」中选择一个要使用的账号".to_string());
        };
        let ready = match ch {
            Channel::Bilibili => !(bili.uid.is_empty() || bili.access_key.is_empty()),
            Channel::Official => !(official.stoken.is_empty() || official.mid.is_empty()),
        };
        if !ready {
            state.scanning.store(false, Ordering::SeqCst);
            return Err("当前选中渠道的账号未登录，请重新登录".to_string());
        }
        ch
    };

    state.stop.store(false, Ordering::SeqCst);
    let stop = state.stop.clone();
    let scanning = state.scanning.clone();
    let app = app.clone();
    // device_id 会 move 进 ScanArgs，display_name 需在其之前取出。
    let display_name = cfg.effective_display_name();

    tauri::async_runtime::spawn(async move {
        let cleanup = app.clone();
        scan::run_scan(
            app,
            scan::ScanArgs {
                current_channel,
                current_game: cfg.current_game,
                hypergryph: hyper,
                bili,
                official,
                interval_ms: cfg.interval_ms,
                device_id: cfg.device_id,
                clear_clipboard: cfg.clear_clipboard,
                auto_close: cfg.auto_close,
                stop,
                display_name,
            },
        )
        .await;
        scanning.store(false, Ordering::SeqCst);
        // 清理扫码关注区（扫描已结束，残留 ROI 会让脏矩形过滤过于保守）。
        let region = {
            let state = cleanup.state::<AppState>();
            let g = state
                .region
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *g
        };
        if let Some(r) = region {
            crate::duplication::clear_scan_roi(r);
        }
    });
    Ok(())
}

#[tauri::command]
async fn start_scan(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    try_start_scan(&app, &state)
}

#[tauri::command]
fn stop_scan(state: tauri::State<'_, AppState>) {
    state.stop.store(true, Ordering::SeqCst);
}

// ---------- 独立人机验证窗口 ----------

/// 打开独立的人机验证窗口（加载 verify.html，GT4 在完整窗口内渲染）。
#[tauri::command]
async fn open_captcha_window(
    app: tauri::AppHandle,
    captcha_id: String,
    session_id: String,
) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    let url = WebviewUrl::App(
        format!("verify.html?captcha_id={captcha_id}&session_id={session_id}").into(),
    );
    // 已存在则先关闭，避免重复。
    if let Some(w) = app.get_webview_window("captcha") {
        let _ = w.close();
    }
    WebviewWindowBuilder::new(&app, "captcha", url)
        .title("人机验证")
        .inner_size(420.0, 460.0)
        .resizable(true)
        .center()
        .build()
        .map_err(|e| format!("打开验证窗口失败: {e}"))?;
    Ok(())
}

/// 验证窗口回传结果：aigis 非空 = 成功；空 = 用户关闭/失败。
#[tauri::command]
fn captcha_result(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    aigis: String,
) -> Result<(), String> {
    *state
        .captcha_result
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(aigis);
    // 关闭验证窗口。
    if let Some(w) = app.get_webview_window("captcha") {
        let _ = w.close();
    }
    let _ = app.emit("captcha-done", ());
    Ok(())
}

/// 前端轮询取验证结果（取走并清空）。
#[tauri::command]
fn take_captcha_result(state: tauri::State<'_, AppState>) -> Option<String> {
    state
        .captcha_result
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

// ---------- 运行入口 ----------

/// 初始化文件日志与 panic 记录。GUI 进程无控制台，闪退 / 异常只有落到文件
/// 才能事后排查（`%APPDATA%\崩坏3扫码器\bh3-scanner.log`，超过 2MB 轮转为 .old）。
fn init_file_logging() {
    let path = AppConfig::path().with_file_name("bh3-scanner.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 2 * 1024 * 1024 {
            let _ = std::fs::rename(&path, path.with_extension("log.old"));
        }
    }
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Info)
            .target(env_logger::Target::Pipe(Box::new(file)))
            .try_init()
            .ok();
    }
    let panic_path = path.clone();
    std::panic::set_hook(Box::new(move |info| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let text = format!("[panic] t={secs} {info}\n");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&panic_path)
        {
            use std::io::Write as _;
            let _ = f.write_all(text.as_bytes());
        }
        log::error!("{text}");
    }));
}

pub fn run() {
    init_file_logging();

    let mut cfg = AppConfig::load();
    if cfg.device_id.is_empty() {
        cfg.device_id = Uuid::new_v4().to_string();
        let _ = cfg.save();
    }
    let win_pos = (cfg.win_x, cfg.win_y);

    let state = AppState {
        stop: Arc::new(AtomicBool::new(false)),
        scanning: Arc::new(AtomicBool::new(false)),
        config: Mutex::new(cfg),
        login_qrs: Mutex::new(QrLoginSlots::default()),
        login_qr_seq: Mutex::new(0),
        region: Mutex::new(None),
        scan_success_times: Mutex::new(Vec::new()),
        captcha_result: Mutex::new(None),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            frontend_log,
            refresh_official_unames,
            select_account,
            delete_account,
            set_account_meta,
            reorder_accounts,
            switch_game,
            login_qr_start,
            login_qr_poll,
            login_qr_cancel,
            hypergryph_send_code,
            hypergryph_login,
            cred_send_code,
            cred_login_sms,
            cred_login_pwd,
            hypergryph_select,
            hypergryph_delete,
            hypergryph_set_meta,
            start_scan,
            stop_scan,
            open_captcha_window,
            captcha_result,
            take_captcha_result,
            capture_preview,
            capture_tiles,
            set_tile_mode,
            set_scan_region,
            set_preview_active,
        ])
        .on_window_event(|window, event| match event {
            // 关闭主窗口 = 退出程序。退出前记住主窗口位置。
            tauri::WindowEvent::CloseRequested { .. } if window.label() == "main" => {
                save_window_position(window.app_handle());
                log::info!("主窗口关闭，退出");
                window.app_handle().exit(0);
            }
            // WebView2 已知问题：最小化还原后首次点击可能被「激活点击」吞掉
            //（表现为标题栏按钮/拖拽一时失灵）。窗口获得焦点时主动回焦 webview。
            tauri::WindowEvent::Focused(true) if window.label() == "main" => {
                let _ = window.set_focus();
            }
            // 仅允许上下调节高度：锁定宽度 300（逻辑像素），高度下限 400。
            tauri::WindowEvent::Resized { .. } if window.label() == "main" => {
                let sf = window.scale_factor().unwrap_or(1.0);
                let size = window.outer_size().unwrap_or_default();
                let lw = size.width as f64 / sf;
                let lh = size.height as f64 / sf;
                if (lw - 300.0).abs() > 0.5 || lh < 400.0 {
                    let _ = window.set_size(tauri::LogicalSize::new(300.0, lh.max(400.0)));
                }
            }
            _ => {}
        })
        .setup(move |app| {
            // 恢复主窗口位置。
            if let (Some(x), Some(y)) = win_pos {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
                }
            }
            let handle = app.handle().clone();
            let state = app.state::<AppState>();
            if state
                .config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .always_on_top
            {
                apply_always_on_top(&handle, true);
            }
            // 启动即预校验当前 B 服账号（若当前渠道为 B 服）。
            prewarm_bili_account(&handle);
            // 仅允许上下调节高度：min/max 尺寸约束（min 宽=max 宽=300 逻辑像素、高 400~20000）。
            // 用系统约束而非 Resized 事件里逐帧 set_size，避免拖拽过程中的抖动。
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_min_size(Some(tauri::LogicalSize::new(300.0, 400.0)));
                let _ = w.set_max_size(Some(tauri::LogicalSize::new(300.0, 20000.0)));
            }
            // 窗口四角圆角（Win11 DWM；Win10 无此 API 静默忽略）。
            #[cfg(windows)]
            {
                use windows::Win32::Graphics::Dwm::{
                    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
                };
                if let Some(w) = app.get_webview_window("main") {
                    if let Ok(hwnd) = w.hwnd() {
                        let pref = DWMWCP_ROUND;
                        let _ = unsafe {
                            DwmSetWindowAttribute(
                                windows::Win32::Foundation::HWND(hwnd.0),
                                DWMWA_WINDOW_CORNER_PREFERENCE,
                                &pref as *const _ as *const _,
                                std::mem::size_of::<i32>() as u32,
                            )
                        };
                    }
                }
            }
            // 启动自动登录：使用上次使用的账号并开始扫描（需已登录任一渠道）。
            if state
                .config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .auto_login_last
            {
                let has_account = {
                    let cfg = state
                        .config
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    {
                        let g = game_key(cfg.current_game);
                        cfg.current_uid(&g).is_some()
                    }
                };
                if has_account {
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                        let state = handle.state::<AppState>();
                        match try_start_scan(&handle, &state) {
                            Ok(()) => log::info!("启动自动登录已开启"),
                            Err(e) => log::warn!("启动自动登录未开始: {e}"),
                        }
                    });
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("崩坏3扫码器启动失败");
}
