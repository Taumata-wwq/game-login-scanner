use crate::prelude::*;

use crate::config::AppConfig;
use crate::AppState;

#[tauri::command]
pub(crate) fn get_config(state: tauri::State<'_, AppState>) -> AppConfig {
    state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// 设置项增量更新补丁（None = 不修改该项；字段名与前端 patch 键一致）。
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct OptionsPatch {
    pub(crate) auto_login_last: Option<bool>,
    pub(crate) always_on_top: Option<bool>,
    pub(crate) auto_close: Option<bool>,
    pub(crate) display_name: Option<String>,
    pub(crate) stop_others_on_scan_success: Option<bool>,
    pub(crate) auto_check_update: Option<bool>,
    // 屏幕扫码 / 通知四项（仅移动端 UI 暴露）：缺字段即不改动，默认语义为 true。
    pub(crate) screen_scan_auto_bg: Option<bool>,
    pub(crate) screen_scan_stop_on_resume: Option<bool>,
    pub(crate) notify_persistent: Option<bool>,
    pub(crate) notify_live_status: Option<bool>,
}

/// 字段级更新设置项（不触碰账号列表）。
///
/// 前端设置项（自动登录/置顶/自动退出/显示帧率/登入名）走 save_config 整份回写（该命令已因该竞态删除），
/// 其 payload 是可能已过时的前端快照 → 与后端 select/delete 等账号命令并发时会互相
/// 覆盖（后到的旧快照抹掉账号改动）。改为只提交本次变更的字段，从根上消除该竞态。
#[tauri::command]
pub(crate) fn set_options(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    patch: OptionsPatch,
) -> Result<(), String> {
    let OptionsPatch {
        auto_login_last,
        always_on_top,
        auto_close,
        display_name,
        stop_others_on_scan_success,
        auto_check_update,
        screen_scan_auto_bg,
        screen_scan_stop_on_resume,
        notify_persistent,
        notify_live_status,
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
    if let Some(v) = screen_scan_auto_bg {
        cfg.screen_scan_auto_bg = v;
    }
    if let Some(v) = screen_scan_stop_on_resume {
        cfg.screen_scan_stop_on_resume = v;
    }
    if let Some(v) = notify_persistent {
        cfg.notify_persistent = v;
    }
    if let Some(v) = notify_live_status {
        cfg.notify_live_status = v;
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

/// 记住界面列宽（侧栏 / 游戏列；拖动结束时调用）。
#[tauri::command]
pub(crate) fn set_ui_widths(
    sidebar: Option<u32>,
    game_col: Option<u32>,
    state: tauri::State<'_, AppState>,
) {
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
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
}

/// 返回应用版本（单一来源：Cargo.toml version）。
#[tauri::command]
pub(crate) fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
