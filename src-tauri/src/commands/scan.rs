use crate::prelude::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::scan;
use crate::AppState;

/// 设置扫描区域（虚拟桌面绝对坐标，物理像素）；w/h 非正表示恢复全屏。
#[tauri::command]
pub(crate) fn set_scan_region(
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
pub(crate) fn prewarm_bili_account(app: &tauri::AppHandle) {
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

/// 扫描任务的收尾守卫：注销注册、解除直播保活、清理扫码 ROI。
///
/// 这些清理写在 `run_scan(..).await` 之后，只有正常返回才会执行；一旦解码/
/// 解析 panic（unwind），目标会永远留在 `scans` 里，之后每次 start_scan 都返回
/// 「该目标已在扫描中」，直播会话也会一直后台拉流。放进 Drop 后异常路径同样收尾。
struct ScanCleanupGuard {
    app: tauri::AppHandle,
    key: String,
}

impl Drop for ScanCleanupGuard {
    fn drop(&mut self) {
        let state = self.app.state::<AppState>();
        {
            let mut scans = state
                .scans
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            scans.remove(&self.key);
        }
        if let Some(id) = self.key.strip_prefix("live:") {
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
    }
}

/// 校验凭据并启动**一路**扫描（目标 = `"screen"` / `"camera"` / `"live:{会话id}"`）。
///
/// 支持并行：最多 5 路直播 + 1 路桌面 + 1 路摄像头；同一目标重复启动报错。
/// 直播目标会**保活**该会话（非主画面也保持拉流/解码，停止时解除）。
pub(crate) fn try_start_scan(
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
    // 第三方渠道账号（选中后优先于 B服/官服；确认时以其渠道凭证换 combo 凭证）。
    let channel_account = cfg.active_channel_account(&game).cloned();
    // 鹰角游戏用鹰角账号；米哈游系用 game_channel 对应的账号。
    let current_channel = if cfg.current_game.is_hypergryph() {
        let ready = !hyper.phone.is_empty() && !hyper.token.is_empty();
        if !ready {
            return Err("请先在「账号」中添加鹰角账号（手机号验证码登录）".to_string());
        }
        // 鹰角游戏用 Official 占位（下游只对米哈游码生效，鹰角码走独立分支）。
        Channel::Official
    } else if channel_account.is_some() {
        // 已选中第三方渠道账号：确认走 ScanContext 的渠道分支，此处仅作占位
        // （下游 current_ready / confirm 都以 channel_account 为准）。
        Channel::Bilibili
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
    // device_id 会 move 进 ScanArgs，display_name 需在其取出。
    let display_name = cfg.effective_display_name();
    let key_for_cleanup = target_key.clone();

    tauri::async_runtime::spawn(async move {
        // RAII 收尾：成功/停止/失败/panic（unwind）都会走到 Drop。
        let _cleanup = ScanCleanupGuard {
            app: app.clone(),
            key: key_for_cleanup,
        };
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
                channel_account,
            },
        )
        .await;
    });
    Ok(())
}

#[tauri::command]
pub(crate) async fn start_scan(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    target: String,
) -> Result<(), String> {
    try_start_scan(&app, &state, target)
}

/// 停止一路扫描（目标 = `"screen"` / `"live:{id}"`；未在运行则静默忽略）。
#[tauri::command]
pub(crate) fn stop_scan(state: tauri::State<'_, AppState>, target: String) {
    let scans = state
        .scans
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(stop) = scans.get(&target) {
        stop.store(true, Ordering::SeqCst);
    }
}

// 人机验证（极验 GT3/GT4）在主 webview 内的 #captchaOverlay 里直接渲染。
// 早期版本另开一个 WebviewWindow（标签 "captcha"）加载 src/verify.html，Android 上
// wry 只有单个 Activity/WebView，第二窗口既渲染不出内容也无法关闭——故整套
// open_captcha_window / captcha_result / take_captcha_result 与 AppState.captcha_result
// 一并移除，src/verify.html 同时删除（逻辑已并入 src/app.js 的 showCaptcha）。

// ---------- 运行入口 ----------
