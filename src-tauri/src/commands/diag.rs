/// 密钥保护往返探针（真机验证 DPAPI/Keystore 可用）。
#[tauri::command]
pub(crate) fn probe_secret(plain: String) -> String {
    let enc = crate::crypto::protect_secret(&plain);
    let dec = crate::crypto::unprotect_secret(&enc);
    format!(
        "加密={enc}
解密={dec}
往返一致={}",
        dec == plain
    )
}

/// TLS 诊断探针（真机排查 mihoyo 域名的 rustls 握手被拒）。
#[tauri::command]
pub(crate) fn probe_tls(host: String, port: u16) -> String {
    crate::http::probe_tls(&host, port)
}

/// HTTP 探针（走完整请求路径：Android 上 mihoyo 域名经系统 TLS 桥）。
#[tauri::command]
pub(crate) fn probe_http(url: String) -> String {
    match crate::http::probe_http(&url) {
        Ok(s) => s,
        Err(e) => format!("请求失败: {e}"),
    }
}

/// 用系统默认程序打开 https 链接（项目信息 / 更新页用；仅允许 https）。
///
/// Android 走屏幕插件新增的 `openUrl` 命令（ACTION_VIEW）；桌面端走
/// `rundll32 url.dll,FileProtocolHandler`。
#[tauri::command]
pub(crate) async fn open_url(url: String) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("仅支持 https 链接".to_string());
    }
    #[cfg(target_os = "android")]
    {
        let h = crate::screen_capture_plugin::handle().ok_or("屏幕捕获插件未注册")?;
        h.run_mobile_plugin_async::<serde_json::Value>("openUrl", serde_json::json!({ "url": url }))
            .await
            .map(|_| ())
            .map_err(|e| format!("打开链接失败：{e}"))
    }
    #[cfg(not(target_os = "android"))]
    {
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &url])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("打开链接失败: {e}"))
    }
}
