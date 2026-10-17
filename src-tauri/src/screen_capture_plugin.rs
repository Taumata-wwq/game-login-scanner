//! 屏幕捕获移动插件装配（Android 真实现；其他平台空装配）。
//!
//! 与 Kotlin 侧 `com.taumata.scanner.plugins.ScreenCapturePlugin` 对应：
//! - `setup` 时经 `register_android_plugin` 由 Activity 实例化 Kotlin 插件并
//!   注册到 `PluginManager`（tauri 2.11 官方移动插件机制）；
//! - `PluginHandle` 存入进程级静态，供本文件命令层 `run_mobile_plugin_async` 调用。
//!
//! 设计说明：本模块**跨平台编译**（非 Android 平台为空装配、命令返回明确错误），
//! 确保 lib.rs 的装配链与命令注册不受 cfg 影响——`crate::android::*` 模块
//! 整体受 `cfg(target_os = "android")` 门控，不能出现在跨平台代码路径上。

#[cfg(target_os = "android")]
use std::sync::OnceLock;
#[cfg(target_os = "android")]
use tauri::plugin::PluginHandle;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::Wry;

/// Kotlin 插件类位置（与 scripts/android/patch-gen.ps1 的同步目标目录一致）。
#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "com.taumata.scanner.plugins";
#[cfg(target_os = "android")]
const PLUGIN_CLASS: &str = "ScreenCapturePlugin";

/// Kotlin 插件句柄（Android 注册后写入；其他平台恒空）。
#[cfg(target_os = "android")]
static HANDLE: OnceLock<PluginHandle<Wry>> = OnceLock::new();

/// 注册屏幕捕获插件（lib.rs 的 `run()` 装配链上调用）。
pub fn init() -> TauriPlugin<Wry> {
    Builder::<Wry, ()>::new("screen-capture")
        .setup(|_app, api| {
            #[cfg(target_os = "android")]
            {
                let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, PLUGIN_CLASS)?;
                let _ = HANDLE.set(handle);
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = api;
            }
            Ok(())
        })
        .build()
}

/// Kotlin 插件句柄（未注册 / 非 Android 平台为 None）。
#[cfg(target_os = "android")]
pub fn handle() -> Option<&'static PluginHandle<Wry>> {
    HANDLE.get()
}

/// 应用句柄（启动时写入；供 JNI 回调线程向前端广播事件）。
#[cfg(target_os = "android")]
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// 记录应用句柄（lib.rs 的 `setup` 中调用）。
#[cfg(target_os = "android")]
pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP.set(app);
}

/// 广播「屏幕共享已被系统/用户停止」。
///
/// 由 JNI 帧桥的 `onCaptureStopped` 调用（Kotlin `MediaProjection.Callback.onStop`）。
/// 用途：Android 14 起每次授权都是一次性令牌，停止后必须重新弹授权框；前端据此
/// 把设置面板状态改回「未开启」并提示，避免界面显示「共享中」却扫不到任何东西。
#[cfg(target_os = "android")]
pub fn notify_capture_stopped() {
    use tauri::Emitter;
    if let Some(app) = APP.get() {
        let _ = app.emit("capture-stopped", ());
    }
}

/// 非 Android 平台：无 MediaProjection，空实现（保持调用点跨平台可编译）。
#[cfg(not(target_os = "android"))]
pub fn notify_capture_stopped() {}

/// 请求屏幕捕获授权（Android）：拉起系统 MediaProjection 对话框，授权成功后
/// 由前台服务建立帧管道。用户取消或拒绝时返回错误。
#[tauri::command]
pub async fn request_screen_capture() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let h = handle().ok_or("屏幕捕获插件未注册")?;
        h.run_mobile_plugin_async::<serde_json::Value>("requestCapture", ())
            .await
            .map(|_| ())
            .map_err(|e| format!("请求屏幕捕获失败：{e}"))
    }
    #[cfg(not(target_os = "android"))]
    {
        Err("当前平台不支持屏幕捕获请求".to_string())
    }
}

/// 停止屏幕捕获（Android）：停止前台服务与帧管道；其他平台空操作成功。
#[tauri::command]
pub async fn stop_screen_capture() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let h = handle().ok_or("屏幕捕获插件未注册")?;
        h.run_mobile_plugin_async::<serde_json::Value>("stopCapture", ())
            .await
            .map(|_| ())
            .map_err(|e| format!("停止屏幕捕获失败：{e}"))
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(())
    }
}

/// 查询屏幕捕获状态（Android 返回是否正在捕获；其他平台恒 false）。
#[tauri::command]
pub async fn screen_capture_state() -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Deserialize)]
        struct CaptureState {
            capturing: bool,
        }
        let h = handle().ok_or("屏幕捕获插件未注册")?;
        let s: CaptureState = h
            .run_mobile_plugin_async("captureState", ())
            .await
            .map_err(|e| format!("查询屏幕捕获状态失败：{e}"))?;
        Ok(s.capturing)
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(false)
    }
}
