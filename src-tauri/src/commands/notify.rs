/// 显示/更新后台常驻通知。
///
/// `target`：点击通知后要定位的目标（`live:<platform>:<room>`；空则只回主界面）。
#[tauri::command]
pub(crate) fn notify_persistent_show(title: String, text: String, target: Option<String>) {
    #[cfg(target_os = "android")]
    crate::android::notify::show_persistent(&title, &text, target.as_deref());
    #[cfg(not(target_os = "android"))]
    {
        let _ = (title, text, target);
    }
}

/// 撤下后台常驻通知（回到前台时调用）。
#[tauri::command]
pub(crate) fn notify_persistent_hide() {
    #[cfg(target_os = "android")]
    crate::android::notify::hide_persistent();
}

/// 发一条可划掉的即时通知。`kind` 决定固定 id（同类事件覆盖，避免刷屏）。
///
/// `target`：点击通知后要定位的目标（`live:<platform>:<room>`；空则只回主界面）。
#[tauri::command]
pub(crate) fn notify_event(kind: String, title: String, text: String, target: Option<String>) {
    #[cfg(target_os = "android")]
    {
        let id = match kind.as_str() {
            "scan_success" => crate::android::notify::ids::SCAN_SUCCESS,
            "live_status" => crate::android::notify::ids::LIVE_STATUS,
            _ => crate::android::notify::ids::SCAN_ERROR,
        };
        crate::android::notify::notify_once(id, &title, &text, target.as_deref());
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (kind, title, text, target);
    }
}

/// 请求通知权限（Android 13+ 必需；其他平台/低版本直接成功）。
///
/// POST_NOTIFICATIONS 在清单里已声明，但从未在运行时请求——Android 13 起默认拒绝，
/// 不请求就发不出任何通知。前端在首次打开设置面板时调用（幂等）。
#[tauri::command]
pub(crate) async fn request_notify_permission() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let h = crate::screen_capture_plugin::handle().ok_or("屏幕捕获插件未注册")?;
        h.run_mobile_plugin_async::<serde_json::Value>(
            "requestNotifyPermission",
            serde_json::json!({}),
        )
        .await
        .map(|_| ())
        .map_err(|e| format!("请求通知权限失败：{e}"))
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(())
    }
}
