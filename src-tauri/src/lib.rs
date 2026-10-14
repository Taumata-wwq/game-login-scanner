//! 游戏扫码器库入口：模块声明 + Tauri 命令 + 运行时装配。

pub mod clipboard;
pub mod config;
pub mod constants;
pub mod crypto;
pub mod decode;
pub mod douyin;
pub mod duplication;
pub mod hls;
pub mod http;
pub mod live;
pub mod mf_decode;
pub mod qr;
pub mod scan;
pub mod screenshot;
pub mod sdk;
pub mod watch;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};
/// 生成 UUID-v4 形态的随机串（seed_id 用）。
///
/// 不引入 uuid crate：seed 只需「每次随机 + UUID 形态」（服务端不校验版本位，
/// 原实现即为 `Uuid::new_v4()`），用已有的 rand 直接生成 16 字节并按 v4 规范
/// 置版本/变体位。
fn random_uuid_v4() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}

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
    /// 并行扫码任务注册表：key = `"screen"` / `"live:{会话id}"` → 停止标志。
    /// 支持同时多路（最多 5 路直播 + 1 路桌面），各自独立启停。
    pub scans: Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>,
    /// 跨扫码目标共享的「已尝试票据」集（同一码被多路发现时不重复确认）。
    pub tried_tickets: Arc<Mutex<std::collections::HashSet<String>>>,
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
    let c = cfg;
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

/// 设置项增量更新补丁（None = 不修改该项；字段名与前端 patch 键一致）。
#[derive(serde::Deserialize, Default)]
struct OptionsPatch {
    auto_login_last: Option<bool>,
    always_on_top: Option<bool>,
    auto_close: Option<bool>,
    display_fps: Option<u32>,
    display_name: Option<String>,
    stop_others_on_scan_success: Option<bool>,
    auto_check_update: Option<bool>,
}

/// 字段级更新设置项（不触碰账号列表）。
///
/// 前端设置项（自动登录/置顶/自动退出/显示帧率/登入名）此前走 save_config 整份回写，
/// 其 payload 是可能已过时的前端快照 → 与后端 select/delete 等账号命令并发时会互相
/// 覆盖（后到的旧快照抹掉账号改动）。改为只提交本次变更的字段，从根上消除该竞态。
#[tauri::command]
fn set_options(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    patch: OptionsPatch,
) -> Result<(), String> {
    let OptionsPatch {
        auto_login_last,
        always_on_top,
        auto_close,
        display_fps,
        display_name,
        stop_others_on_scan_success,
        auto_check_update,
    } = patch;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(v) = auto_login_last {
        cfg.auto_login_last = v;
    }
    if let Some(v) = auto_close {
        cfg.auto_close = v;
    }
    if let Some(v) = stop_others_on_scan_success {
        cfg.stop_others_on_scan_success = v;
    }
    if let Some(v) = auto_check_update {
        cfg.auto_check_update = v;
    }
    if let Some(v) = display_fps {
        cfg.display_fps = config::normalize_display_fps(v);
    }
    if let Some(v) = display_name {
        cfg.display_name = v;
    }
    let on_top_changed = always_on_top.filter(|v| *v != cfg.always_on_top);
    if let Some(v) = always_on_top {
        cfg.always_on_top = v;
    }
    cfg.save().map_err(|e| e.to_string())?;
    drop(cfg);
    if let Some(v) = on_top_changed {
        apply_always_on_top(&app, v);
    }
    Ok(())
}

/// 前端日志落盘：窗口操作失败等前端异常写入日志文件，便于远程排查。
#[tauri::command]
fn frontend_log(level: String, msg: String) {
    match level.as_str() {
        "error" => log::error!("[web] {msg}"),
        "warn" => log::warn!("[web] {msg}"),
        _ => log::info!("[web] {msg}"),
    }
}

/// 记住「最后观看的直播间」（`{platform}:{room}`；空串 = 回到桌面）。
/// 切换主画面 / 退出直播模式时调用；下次启动据此恢复。
#[tauri::command]
fn set_last_live(id: String, state: tauri::State<'_, AppState>) {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cfg.last_live_id != id {
        cfg.last_live_id = id;
        let _ = cfg.save();
    }
}

/// 记住界面列宽（侧栏 / 游戏列；拖动结束时调用）。
#[tauri::command]
fn set_ui_widths(sidebar: Option<u32>, game_col: Option<u32>, state: tauri::State<'_, AppState>) {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(v) = sidebar {
        cfg.sidebar_w = Some(v);
    }
    if let Some(v) = game_col {
        cfg.game_col_w = Some(v);
    }
    let _ = cfg.save();
}

/// 返回应用版本（单一来源：Cargo.toml version）。
#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 用系统默认程序打开 https 链接（项目信息 / 更新页用；仅允许 https）。
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("仅支持 https 链接".to_string());
    }
    std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", &url])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("打开链接失败: {e}"))
}

/// 解析语义化版本号为数值三元组（接受 "v1.2.3" / "1.2" / "1.2.3-rc1" 等）。
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let mut it = s.split(['.', '-', '+']);
    let a = it.next()?.parse::<u64>().ok()?;
    let b = it.next().unwrap_or("0").parse::<u64>().unwrap_or(0);
    let c = it.next().unwrap_or("0").parse::<u64>().unwrap_or(0);
    Some((a, b, c))
}

#[cfg(test)]
mod update_tests {
    use super::parse_version;

    #[test]
    fn version_parse_and_compare() {
        assert_eq!(parse_version("0.3.2"), Some((0, 3, 2)));
        assert_eq!(parse_version("v0.3.2"), Some((0, 3, 2)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("v0.4.0-rc1"), Some((0, 4, 0)));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("abc"), None);
        // 新版本判定（与 check_update 的 `latest <= cur` 逻辑一致）
        assert!(parse_version("v0.3.3").unwrap() > parse_version("0.3.2").unwrap());
        assert!(parse_version("v0.3.1").unwrap() < parse_version("0.3.2").unwrap());
        assert!(parse_version("v1.0.0").unwrap() > parse_version("0.9.9").unwrap());
    }
}

/// 检查 GitHub Release 更新（正式版）。
///
/// - `manual=false`（启动自动检查）：无更新 / 失败均静默（仅写调试日志）；
/// - `manual=true`（设置面板「检查更新」）：通过 `update-status` 事件反馈结果。
/// - 发现新正式版：下载安装包 → 启动安装程序 → 关闭当前程序。
#[tauri::command]
fn check_update(app: tauri::AppHandle, manual: bool) {
    std::thread::spawn(move || {
        let emit = |kind: &str, message: String| {
            let _ = app.emit(
                "update-status",
                serde_json::json!({ "kind": kind, "message": message }),
            );
        };
        let current = env!("CARGO_PKG_VERSION");
        let headers = vec![
            (
                "User-Agent".to_string(),
                "game-login-scanner-updater".to_string(),
            ),
            (
                "Accept".to_string(),
                "application/vnd.github+json".to_string(),
            ),
        ];
        // GitHub 的 /latest：最新「非草稿、非预发布」的正式版 Release。
        let resp = match crate::http::request_sync(
            "GET",
            "https://api.github.com/repos/Taumata-wwq/game-login-scanner/releases/latest",
            &headers,
            None,
        ) {
            Ok(r) => r,
            Err(e) => {
                log::debug!("检查更新失败: {e}");
                if manual {
                    emit("error", format!("检查更新失败：{e}"));
                }
                return;
            }
        };
        let v: serde_json::Value = match serde_json::from_str(&resp.body) {
            Ok(v) => v,
            Err(e) => {
                log::debug!("检查更新响应解析失败: {e}");
                if manual {
                    emit("error", format!("检查更新失败（响应解析）：{e}"));
                }
                return;
            }
        };
        // GitHub 对匿名请求访问私有仓库返回 404（"Not Found"）；其它 message 原样带出，
        // 便于区分「仓库不可访问 / 限流 / 无 Release」等情形。
        if let Some(msg) = v["message"].as_str() {
            let hint = if resp.status == 404 {
                "（仓库不可访问：可能为私有仓库或尚无 Release）"
            } else {
                ""
            };
            log::debug!("检查更新 API 返回 {}: {msg}", resp.status);
            if manual {
                emit(
                    "error",
                    format!("检查更新失败：HTTP {} {msg}{hint}", resp.status),
                );
            }
            return;
        }
        let tag = v["tag_name"].as_str().unwrap_or("").to_string();
        let (Some(latest), Some(cur)) = (parse_version(&tag), parse_version(current)) else {
            if manual {
                emit("error", "检查更新失败：版本号无法解析".to_string());
            }
            return;
        };
        if latest <= cur {
            if manual {
                emit("info", format!("已是最新版本（v{current}）"));
            }
            return;
        }
        // 新版本：定位安装包资源（历史命名含 `*-setup.exe` 与 `*_x64-setup.exe`
        // 两种形态 → 统一按「以 setup.exe 结尾」匹配，避免漏选）。
        let asset = v["assets"].as_array().and_then(|a| {
            a.iter()
                .find(|x| {
                    x["name"]
                        .as_str()
                        .map(|n| n.to_ascii_lowercase().ends_with("setup.exe"))
                        .unwrap_or(false)
                })
                .and_then(|x| {
                    Some((
                        x["name"].as_str().unwrap_or("setup.exe").to_string(),
                        x["browser_download_url"].as_str()?.to_string(),
                    ))
                })
        });
        let Some((_, url)) = asset else {
            emit(
                "error",
                format!("发现新版本 {tag}，但 Release 中没有安装包资源"),
            );
            return;
        };
        emit("info", format!("发现新版本 {tag}，正在下载安装包…"));
        let bytes = match crate::http::request_bytes_sync("GET", &url, &headers, None) {
            Ok((_, _, b)) => b,
            Err(e) => {
                emit("error", format!("下载安装包失败：{e}"));
                return;
            }
        };
        let path = std::env::temp_dir().join(format!("game-login-scanner-{tag}-setup.exe"));
        if let Err(e) = std::fs::write(&path, &bytes) {
            emit("error", format!("保存安装包失败：{e}"));
            return;
        }
        emit("info", "下载完成，正在启动安装程序…".to_string());
        if let Err(e) = std::process::Command::new(&path).spawn() {
            emit("error", format!("启动安装包失败：{e}"));
            return;
        }
        // 给安装器一点启动时间，随后关闭当前窗口（关闭路径会保存窗口几何并退出）。
        std::thread::sleep(std::time::Duration::from_millis(800));
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(w) = handle.get_webview_window("main") {
                let _ = w.close();
            }
        });
    });
}

/// 应用窗口置顶设置。
fn apply_always_on_top(app: &tauri::AppHandle, on_top: bool) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_always_on_top(on_top);
    }
}

/// 记住主窗口当前坐标与尺寸（关闭 / 移动 / 缩放后调用，供下次启动恢复）。
///
/// **最小化时跳过**：最小化会把窗口挪到 (-32000,-32000) 并缩到 160×28，
/// 若照存，下次启动会恢复成屏幕外的迷你窗口（2026-10-14 实测踩中）。
pub(crate) fn save_window_position(app: &tauri::AppHandle) {
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    if w.is_minimized().unwrap_or(false) {
        return;
    }
    let Ok(pos) = w.outer_position() else {
        return;
    };
    // 防御：仍遇到异常坐标（极小化竞态）时跳过位置保存。
    if pos.x <= -30000 || pos.y <= -30000 {
        return;
    }
    // 尺寸按**逻辑像素**存储（物理 ÷ scale），与恢复端 `LogicalSize` 对齐。
    let scale = w.scale_factor().unwrap_or(1.0);
    let size = w.inner_size().ok().map(|s| {
        (
            (s.width as f64 / scale).round().max(1.0) as u32,
            (s.height as f64 / scale).round().max(1.0) as u32,
        )
    });
    let state = app.state::<AppState>();
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.win_x = Some(pos.x);
    cfg.win_y = Some(pos.y);
    // 防御：异常小尺寸（<400 宽）不写入，避免恢复成迷你窗口。
    if let Some((wq, hq)) = size {
        if wq >= 400 && hq >= 300 {
            cfg.win_w = Some(wq);
            cfg.win_h = Some(hq);
        }
    }
    let _ = cfg.save();
}

/// 防抖版窗口几何保存：拖动/缩放事件密集，最多每 800ms 落盘一次；
/// 事件停止后再**尾部补存**一次，避免「最后一次事件的值」丢失
/// （2026-10-14 实测：无尾部补偿时拉伸后的最终尺寸只存到中途值）。
fn save_window_geometry_throttled(app: &tauri::AppHandle) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    static PENDING: AtomicBool = AtomicBool::new(false);
    let now = std::time::Instant::now();
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let due = last
        .map(|t| t.elapsed() >= std::time::Duration::from_millis(800))
        .unwrap_or(true);
    if due {
        *last = Some(now);
        drop(last);
        save_window_position(app);
        return;
    }
    drop(last);
    // 尾部补偿：已有一个待补线程在跑则跳过（其保存时点也覆盖后续事件的值）。
    if !PENDING.swap(true, Ordering::SeqCst) {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(900));
            PENDING.store(false, Ordering::SeqCst);
            *LAST
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(std::time::Instant::now());
            save_window_position(&app);
        });
    }
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
        Channel::Official => {
            cfg.remove_official(&game, &uid);
            // 同步清掉该账号的 bh_info 缓存（缓存带 uid 键，本不会串号，
            // 但账号已删除就无需再占着槽位）。
            crate::sdk::mihoyo::clear_bh_info_cache(&uid);
        }
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
                if t.game == game {
                    idx(&t.uid)
                } else {
                    usize::MAX
                }
            });
        }
        Channel::Official => {
            cfg.official_accounts.sort_by_key(|t| {
                if t.game == game {
                    idx(&t.uid)
                } else {
                    usize::MAX
                }
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

/// 重排鹰角账号：仅重排当前游戏的账号，其它游戏保持原相对顺序
/// （与 reorder_accounts 的米哈游账号处理一致）。
#[tauri::command]
fn hypergryph_reorder(
    state: tauri::State<'_, AppState>,
    game: String,
    order: Vec<String>,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let idx = |phone: &str| order.iter().position(|p| p == phone).unwrap_or(usize::MAX);
    cfg.hypergryph_accounts.sort_by_key(|t| {
        if t.game == game {
            idx(&t.phone)
        } else {
            usize::MAX
        }
    });
    cfg.save().map_err(|e| e.to_string())?;
    Ok(())
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
            random_uuid_v4(),
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
    phone: String,
    aigis: Option<String>,
) -> Result<serde_json::Value, String> {
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
        // B站短信登录受风控限制（86103），不支持；登录请用密码登录。
        "bilibili" => Err("B站不支持短信登录，请使用密码登录".to_string()),
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
        "bilibili" => return Err("B站不支持短信登录，请使用密码登录".to_string()),
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
            sdk::cred_login::mihoyo_login_pwd(&device_id, &device_fp, &account, &password, &aigis)
                .await?
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

// ---------- 扫码登录（账号登录二维码） ----------

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
            let official =
                sdk::official::qr_login_start(&device_id, constants::DSPROD_DEVICE_FP_DEFAULT)
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

/// 预览 GDI 兜底缓存：桌面复制不可用时降级（内部限频 60fps），
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
    // 取帧 / BGRA→RGB / 节流全程同步且可能耗时（DXGI Map 数毫秒），放到 blocking
    // 线程池执行，避免占住 tokio async 工作线程（P1-16）。
    tauri::async_runtime::spawn_blocking(move || capture_preview_sync(&app, x, y, w, h, max_width))
        .await
        .map_err(|e| format!("预览任务失败: {e}"))?
}

/// `capture_preview` 的同步实现（在 blocking 池执行）。
fn capture_preview_sync(
    app: &tauri::AppHandle,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    max_width: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    // 用带 1 秒缓存的 screen_rects() 枚举显示器（每帧实时枚举是预览帧率上不去的瓶颈之一）。
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
            // GDI 兜底：限频 60fps（16ms），未到间隔时沿用缓存帧。
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
async fn capture_tiles(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    max_width: u32,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || capture_tiles_sync(&app, x, y, w, h, max_width))
        .await
        .map_err(|e| format!("瓦片任务失败: {e}"))?
}

/// `capture_tiles` 的同步实现（在阻塞池执行；拖拽时 30Hz 调用，含整屏降采样）。
fn capture_tiles_sync(
    app: &tauri::AppHandle,
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
            Some((
                pos.x + size.width as i32 / 2,
                pos.y + size.height as i32 / 2,
            ))
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
    let (rx, ry, rw, rh) = if w > 0 && h > 0 {
        (x, y, w, h)
    } else {
        (dx, dy, mw, mh)
    };
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

// ---------- 直播流（B站低延迟捕获） ----------

/// 启动直播会话：解析平台直播间为 http-flv 流并开始解码，
/// 阻塞至拿到首帧。返回会话信息（含 id，供 capture_live_frame 用）。
/// `platform` 为 "bilibili"（默认）或 "douyin"。多路监控：每房间一个会话。
#[tauri::command]
async fn live_start(platform: String, room: String) -> Result<crate::live::SessionInfo, String> {
    let p = crate::live::Platform::parse(&platform);
    crate::live::start(p, room).await
}

/// 停止指定直播会话；room 为空则停止全部（预览恢复屏幕画面，扫码恢复屏幕扫描）。
#[tauri::command]
fn live_stop(platform: Option<String>, room: Option<String>) {
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
fn live_list() -> Vec<crate::live::SessionInfo> {
    crate::live::list_sessions()
}

/// 设置前台（主画面）会话：恢复其拉流、暂停其余（后台零带宽零解码）。
#[tauri::command]
fn live_set_active(id: String) {
    if id.is_empty() {
        crate::live::pause_all();
    } else {
        crate::live::set_active(&id);
    }
}

/// 监测同步的入参（前端传已保存直播间列表）。
#[derive(serde::Deserialize)]
struct WatchRoom {
    platform: String,
    room: String,
}

/// 同步「开播/下播」即时监测列表（B站弹幕 WS，见 `watch.rs`；幂等）。
///
/// 弹幕 WS 在房间状态切换时实时推送 LIVE/PREPARING → `live-status` 事件，
/// 感知延迟从轮询的「≤30s」降到「≈1s」。非 B站（抖音）由轮询兜底。
#[tauri::command]
fn live_watch_sync(rooms: Vec<WatchRoom>, app: tauri::AppHandle) {
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
fn live_history_push(
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
fn live_history_remove(
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
fn live_history_set_note(
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
fn live_history_reorder(
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
#[tauri::command]
async fn live_refresh_meta(
    platform: String,
    room: String,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let plat = crate::live::Platform::parse(&platform);
    let (tx, rx) = std::sync::mpsc::channel();
    let room_for_fetch = room.clone();
    std::thread::spawn(move || {
        let m = crate::live::fetch_meta(plat, &room_for_fetch);
        let _ = tx.send(m);
    });
    let meta = rx.recv().map_err(|_| "元数据获取失败".to_string())?;
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
        let _ = cfg.save();
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
async fn capture_live_frame(
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

/// 校验凭据并启动**一路**扫描（目标 = `"screen"` 或 `"live:{会话id}"`）。
///
/// 支持并行：最多 5 路直播 + 1 路桌面；同一目标重复启动报错。
/// 直播目标会**保活**该会话（非主画面也保持拉流/解码，停止时解除）。
fn try_start_scan(
    app: &tauri::AppHandle,
    state: &AppState,
    target_key: String,
) -> Result<(), String> {
    let target = scan::ScanTarget::parse(&target_key)?;

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
            return Err("请先在「账号」中添加鹰角账号（手机号验证码登录）".to_string());
        }
        // 鹰角游戏用 Official 占位（下游只对米哈游码生效，鹰角码走独立分支）。
        Channel::Official
    } else {
        let Some(ch) = cfg.game_channel.get(&game).copied().flatten() else {
            return Err("请先在「账号」中选择一个要使用的账号".to_string());
        };
        let ready = match ch {
            Channel::Bilibili => !(bili.uid.is_empty() || bili.access_key.is_empty()),
            Channel::Official => !(official.stoken.is_empty() || official.mid.is_empty()),
        };
        if !ready {
            return Err("当前选中渠道的账号未登录，请重新登录".to_string());
        }
        ch
    };

    // 占位 + 上限检查（同一把锁内完成，防快速双击/并发调用的竞态）。
    let stop = {
        let mut scans = state
            .scans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if scans.contains_key(&target_key) {
            return Err("该目标已在扫描中".to_string());
        }
        let live_count = scans.keys().filter(|k| k.starts_with("live:")).count();
        if matches!(target, scan::ScanTarget::Live(_)) && live_count >= 5 {
            return Err("最多同时扫描 5 个直播间（桌面可另算一路）".to_string());
        }
        let stop = Arc::new(AtomicBool::new(false));
        scans.insert(target_key.clone(), stop.clone());
        stop
    };
    // 直播目标：保活该会话（多路并行时后台会话也保持拉流/解码）。
    if let scan::ScanTarget::Live(id) = &target {
        crate::live::set_scan_keepalive(id, true);
    }

    let tried = state.tried_tickets.clone();
    let app = app.clone();
    // device_id 会 move 进 ScanArgs，display_name 需在其之前取出。
    let display_name = cfg.effective_display_name();
    let key_for_cleanup = target_key.clone();

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
                device_id: cfg.device_id,
                auto_close: cfg.auto_close,
                stop,
                display_name,
                target,
                tried_tickets: tried,
            },
        )
        .await;
        // 收尾：解除注册 + 解除保活（成功/停止/失败均经此处）。
        let state = cleanup.state::<AppState>();
        {
            let mut scans = state
                .scans
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            scans.remove(&key_for_cleanup);
        }
        if let Some(id) = key_for_cleanup.strip_prefix("live:") {
            crate::live::set_scan_keepalive(id, false);
        }
        // 清理扫码关注区（扫描已结束，残留 ROI 会让脏矩形过滤过于保守）。
        let region = {
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
    target: String,
) -> Result<(), String> {
    try_start_scan(&app, &state, target)
}

/// 停止一路扫描（目标 = `"screen"` / `"live:{id}"`；未在运行则静默忽略）。
#[tauri::command]
fn stop_scan(state: tauri::State<'_, AppState>, target: String) {
    let scans = state
        .scans
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(stop) = scans.get(&target) {
        stop.store(true, Ordering::SeqCst);
    }
}

// ---------- 独立人机验证窗口 ----------

/// 打开独立的人机验证窗口（加载 verify.html，GT4 在完整窗口内渲染）。
#[tauri::command]
async fn open_captcha_window(
    app: tauri::AppHandle,
    captcha_id: String,
    session_id: String,
    risk_type: Option<String>,
    challenge: Option<String>,
    gt3: Option<bool>,
) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    let risk = risk_type.unwrap_or_default();
    let ch = challenge.unwrap_or_default();
    let g3 = if gt3.unwrap_or(false) { "1" } else { "0" };
    let url = WebviewUrl::App(
        format!(
            "verify.html?captcha_id={captcha_id}&session_id={session_id}&risk_type={risk}&challenge={ch}&gt3={g3}"
        )
        .into(),
    );
    // 已存在则先关闭，避免重复。
    if let Some(w) = app.get_webview_window("captcha") {
        let _ = w.close();
    }
    WebviewWindowBuilder::new(&app, "captcha", url)
        .title("人机验证")
        .inner_size(600.0, 640.0)
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
/// 才能事后排查（`%APPDATA%\游戏扫码器\game-login-scanner.log`，超过 2MB 轮转为 .old）。
fn init_file_logging() {
    let path = AppConfig::path().with_file_name("game-login-scanner.log");
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
        cfg.device_id = random_uuid_v4();
        let _ = cfg.save();
    }
    let win_pos = (cfg.win_x, cfg.win_y);
    let win_size = (cfg.win_w, cfg.win_h);

    let state = AppState {
        scans: Mutex::new(std::collections::HashMap::new()),
        tried_tickets: Arc::new(Mutex::new(std::collections::HashSet::new())),
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
            set_options,
            frontend_log,
            set_last_live,
            set_ui_widths,
            get_app_version,
            open_url,
            check_update,
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
            hypergryph_reorder,
            start_scan,
            stop_scan,
            open_captcha_window,
            captcha_result,
            take_captcha_result,
            capture_preview,
            capture_tiles,
            capture_live_frame,
            live_start,
            live_stop,
            live_list,
            live_set_active,
            live_history_push,
            live_history_remove,
            live_history_reorder,
            live_history_set_note,
            live_refresh_meta,
            live_watch_sync,
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
            // 移动 / 缩放的几何持久化（防抖 800ms；关闭时另有完整保存）。
            tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_)
                if window.label() == "main" =>
            {
                save_window_geometry_throttled(window.app_handle());
            }
            // WebView2 已知问题：最小化还原后首次点击可能被「激活点击」吞掉
            //（表现为标题栏按钮/拖拽一时失灵）。窗口获得焦点时主动回焦 webview。
            tauri::WindowEvent::Focused(true) if window.label() == "main" => {
                let _ = window.set_focus();
            }
            // 验证窗口被用户直接关闭（未通过 captcha_result 回传）时写入空结果，
            // 否则前端轮询 take_captcha_result 永远拿不到值。正常提交已先写结果，
            // 此处仅在仍为空时补写，不覆盖。
            tauri::WindowEvent::CloseRequested { .. } if window.label() == "captcha" => {
                let st = window.app_handle().state::<AppState>();
                let mut r = st
                    .captcha_result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if r.is_none() {
                    *r = Some(String::new());
                }
            }
            // 宽度锁定已移除（多直播间阶段1）：宽度可自由调节，尺寸约束由
            // tauri.conf.json 的 min/max（300×500 ~ 720×980）统一控制。旧逻辑每次
            // Resized 都把宽度设回 300（阶段1 之前遗留），是「只能上下拖、宽度锁死」
            // 的真正根因——commit 38ccb55 仅删除了 setup 中的 set_min/max_size，漏删此处。
            _ => {}
        })
        .setup(move |app| {
            // 恢复主窗口尺寸（逻辑像素；越界值由 min/max 约束自动夹取）与位置。
            // 防御：跳过异常历史值（屏幕外坐标 / 迷你尺寸），避免「看不见的窗口」。
            if let Some(w) = app.get_webview_window("main") {
                if let (Some(wq), Some(hq)) = win_size {
                    if wq >= 400 && hq >= 300 {
                        let _ = w.set_size(tauri::LogicalSize::new(wq as f64, hq as f64));
                    }
                }
                if let (Some(x), Some(y)) = win_pos {
                    if x > -30000 && y > -30000 {
                        let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
                    }
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
            // 窗口尺寸约束统一由 tauri.conf.json 声明（minWidth/maxWidth/
            // minHeight/maxHeight = 300×500 ~ 720×980）。旧的 Rust 级
            // set_min_size/set_max_size（锁宽 300）已移除——它与 conf 的
            // maxWidth 冲突，导致宽度被锁死、只能上下拖（阶段 1 修复）。
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
                        match try_start_scan(&handle, &state, "screen".to_string()) {
                            Ok(()) => log::info!("启动自动登录已开启"),
                            Err(e) => log::warn!("启动自动登录未开始: {e}"),
                        }
                    });
                }
            }
            // 全部初始化就绪后再显示窗口：窗口创建时 visible=false（tauri.conf.json），
            // 至此位置/尺寸/圆角均已设定，一次性在正确位置弹出，避免「居中闪现再跳位」。
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("游戏扫码器启动失败");
}
