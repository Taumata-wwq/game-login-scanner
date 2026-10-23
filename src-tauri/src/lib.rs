//! 游戏扫码器库入口：模块声明 + Tauri 命令 + 运行时装配。

pub mod camera_plugin;
pub mod channel_plugin;
pub mod config;
pub mod constants;
pub mod crypto;
pub mod decode;
pub mod douyin;
pub mod hls;
pub mod http;
pub mod live;
pub mod qr;
pub mod scan;
pub mod screen_capture_plugin;
pub mod sdk;
pub mod watch;

// ---------- Windows 专有模块（DXGI / GDI / 剪贴板 / MF 硬解）----------
#[cfg(windows)]
pub mod clipboard;
#[cfg(windows)]
pub mod duplication;
#[cfg(windows)]
pub mod mf_decode;
#[cfg(windows)]
pub mod screenshot;

// ---------- Android 专有模块（MediaProjection / Camera2 / MediaCodec / Keystore）----------
#[cfg(target_os = "android")]
pub mod android;

// 平台门面：Android 下把 screenshot / duplication / clipboard 重定向到 android::* 实现，
// 使 lib.rs / scan.rs / qr.rs 中的 crate::screenshot::* 等调用点在两平台均可用。
#[cfg(target_os = "android")]
pub use android::{clipboard, duplication, screenshot};

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::Manager;

pub mod commands;

/// 命令模块共用的 prelude：Tauri trait、配置类型与跨领域辅助函数。
///
/// 命令按领域拆开后，这些名字被多个模块同时需要（例如账号、登录、扫码都要
/// \`game_key\` 与 \`AppState\`）；集中在这里是为了让每个命令文件保持短小的抬头，
/// 而不是各自罗列七八个 use。
pub(crate) mod prelude {
    pub(crate) use tauri::{Emitter, Manager};

    pub(crate) use crate::commands::account::{cfg_device_id, game_key};

    pub(crate) use crate::commands::preview::preview_payload;
    pub(crate) use crate::commands::scan::prewarm_bili_account;
    pub(crate) use crate::commands::util::random_uuid_v4;
    pub(crate) use crate::commands::window::apply_always_on_top;

    pub(crate) use crate::config::{AppConfig, Channel};
    pub(crate) use crate::QrLoginSlots;
    pub(crate) use crate::{constants, qr, sdk};
}

use config::AppConfig;
use screen_capture_plugin::{request_screen_capture, screen_capture_state, stop_screen_capture};
use sdk::bili::BiliQrSession;
use sdk::official::OfficialQrSession;

// 命令实现全部在 commands/* 下按领域拆分；这里只保留共享运行时状态、
// 跨命令的工具函数与进程装配（run），并把这些命令统一重导出。
/// 双渠道登录会话槽。
#[derive(Default)]
pub(crate) struct QrLoginSlots {
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
}

pub(crate) use commands::account::*;
pub(crate) use commands::applog::*;
pub(crate) use commands::camera::*;
pub(crate) use commands::config::*;
pub(crate) use commands::diag::*;
pub(crate) use commands::live::*;
pub(crate) use commands::log::*;
pub(crate) use commands::login::*;
pub(crate) use commands::notify::*;
pub(crate) use commands::preview::*;
pub(crate) use commands::scan::*;
pub(crate) use commands::update::*;
pub(crate) use commands::util::*;
pub(crate) use commands::window::*;

// Tauri v2 移动端入口点：Android/iOS 需要由该宏生成 JNI 导出符号
// （否则 .so 校验报 "does not include required runtime symbols"）。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_file_logging();

    let mut cfg = AppConfig::load();
    if cfg.device_id.is_empty() {
        cfg.device_id = random_uuid_v4();
        if let Err(e) = cfg.save() {
            log::warn!("保存配置失败: {e}");
        }
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
    };

    tauri::Builder::default()
        // 屏幕捕获移动插件（Android 真实现；其他平台空装配，见模块文档）。
        .plugin(crate::screen_capture_plugin::init())
        // 摄像头扫码移动插件（Android 真实现；其他平台空装配）。
        .plugin(crate::camera_plugin::init())
        .plugin(crate::channel_plugin::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_options,
            frontend_log,
            notify_persistent_show,
            notify_persistent_hide,
            notify_event,
            request_notify_permission,
            set_last_live,
            set_ui_widths,
            get_app_version,
            probe_tls,
            probe_http,
            probe_secret,
            open_url,
            check_update,
            refresh_official_unames,
            select_account,
            delete_account,
            set_account_meta,
            reorder_accounts,
            switch_game,
            channel_login,
            bili_sdk_login,
            delete_channel_account,
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
            request_screen_capture,
            stop_screen_capture,
            screen_capture_state,
            crate::camera_plugin::request_camera_permission,
            crate::camera_plugin::start_camera_capture,
            crate::camera_plugin::stop_camera_capture,
            crate::camera_plugin::camera_capture_state,
            capture_camera_frame,
            camera_rotation,
            crate::camera_plugin::move_to_back,
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
            // 宽度锁定已移除（多直播间阶段1）：宽度可自由调节，尺寸约束由
            // tauri.conf.json 的 min/max（300×500 ~ 720×980）统一控制。旧逻辑每次
            // Resized 都把宽度设回 300（阶段1遗留），是「只能上下拖、宽度锁死」
            // 的真正根因——commit 38ccb55 仅删除了 setup 中的 set_min/max_size，漏删此处。
            _ => {}
        })
        .setup(move |app| {
            // 记录应用句柄：JNI 帧桥的「捕获被系统停止」回调需要它向前端广播事件。
            #[cfg(target_os = "android")]
            {
                crate::screen_capture_plugin::set_app_handle(app.handle().clone());
            }
            // Android：配置落点必须用应用私有目录（%APPDATA% 不存在、当前目录不可写）。
            // 用 Tauri 的跨平台路径解析器（app_data_dir），此处 app 句柄已就绪。
            // 注入后**重载配置**到 state（run() 阶段的加载用的是默认路径 → 恒默认值）。
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                if crate::config::data_dir().is_none() {
                    if let Ok(dir) = app.path().app_data_dir() {
                        crate::config::set_data_dir(dir);
                        let mut cfg = AppConfig::load();
                        if cfg.device_id.is_empty() {
                            cfg.device_id = random_uuid_v4();
                            if let Err(e) = cfg.save() {
                                log::warn!("保存配置失败: {e}");
                            }
                        }
                        *app.state::<AppState>()
                            .config
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = cfg;
                    } else {
                        // 数据目录解析失败＝本次会话所有配置都不落盘，必须显式告警静默）。
                        log::warn!("应用数据目录解析失败，配置无法持久化");
                    }
                }
            }
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
            //
            // **移动端已移除该设置项**（用户需求）：手机端启动即扫码会在
            // 用户还没选好账号/直播间时就抢占屏幕捕获与解码，弊大于利。故 Android 上
            // 只保留字段的读写兼容（旧配置仍能反序列化），但**不再据此自动开扫**——
            // 否则升级后的旧配置会把已删掉的功能"复活"。桌面端行为不变。
            #[cfg(not(target_os = "android"))]
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
                        // 三方渠道（小米/vivo 等）账号不在 current_uid 的覆盖范围内，
                        // 只判 current_uid 会让这些用户的「启动自动登录」静默失效。
                        cfg.current_uid(&g).is_some() || cfg.active_channel_account(&g).is_some()
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

#[cfg(test)]
mod options_patch_tests {
    use super::OptionsPatch;

    /// 用前端实际提交的全部 11 个键驱动反序列化，逐项断言都被接住。
    /// 少一个键 = 该开关静默不落盘四项移动端开关就是这样失效的）。
    #[test]
    fn all_frontend_option_keys_are_accepted() {
        let json = r#"{
            "auto_login_last": true,
            "always_on_top": false,
            "auto_close": true,
            "display_name": "测试",
            "stop_others_on_scan_success": false,
            "auto_check_update": true,
            "screen_scan_auto_bg": false,
            "screen_scan_stop_on_resume": false,
            "notify_persistent": false,
            "notify_live_status": false
        }"#;
        let p: OptionsPatch =
            serde_json::from_str(json).expect("前端提交的键必须全部被 OptionsPatch 接住");
        assert_eq!(p.auto_login_last, Some(true));
        assert_eq!(p.always_on_top, Some(false));
        assert_eq!(p.auto_close, Some(true));
        assert_eq!(p.display_name.as_deref(), Some("测试"));
        assert_eq!(p.stop_others_on_scan_success, Some(false));
        assert_eq!(p.auto_check_update, Some(true));
        assert_eq!(p.screen_scan_auto_bg, Some(false));
        assert_eq!(p.screen_scan_stop_on_resume, Some(false));
        assert_eq!(p.notify_persistent, Some(false));
        assert_eq!(p.notify_live_status, Some(false));
    }

    /// 未知键必须报错而不是被静默丢弃（静默丢弃正是四开关失效的机制）。
    #[test]
    fn unknown_option_key_is_rejected() {
        let r = serde_json::from_str::<OptionsPatch>(r#"{"nope": true}"#);
        assert!(r.is_err(), "未知键必须让 set_options 失败，而不是悄悄丢掉");
    }
}
