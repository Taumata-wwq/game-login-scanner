use crate::prelude::*;

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
pub(crate) fn check_update(app: tauri::AppHandle, manual: bool) {
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
        // ---- Android：无「下载安装包 → 启动安装器」路径 ----
        //
        // 桌面端下载的是 `*-setup.exe` 并 `Command::new(&path).spawn()` 拉起安装程序；
        // Android 既不能执行 exe，也无法静默安装 APK（需用户逐次确认 + 未知来源授权）。
        // 因此这里只做「告知 + 打开下载页」：把 Release 页面交给系统浏览器，
        // 用户在网页上取 APK 自行安装（避免应用内下载 APK 被渠道判为自更新风险）。
        //
        // **自动检查不弹浏览器**（启动即跳转是骚扰）：只在日志里提示有新版本；
        // 手动检查才把 `url` 交给前端，由前端调 `open_url` 打开（与项目信息链接同一条
        // 路径，复用 Android 侧 ACTION_VIEW 实现）。
        #[cfg(target_os = "android")]
        {
            let page = v["html_url"]
                .as_str()
                .unwrap_or("https://github.com/Taumata-wwq/game-login-scanner/releases/latest")
                .to_string();
            if manual {
                let _ = app.emit(
                    "update-status",
                    serde_json::json!({
                        "kind": "info",
                        "message": format!("发现新版本 {tag}，正在打开下载页（当前 v{current}）…"),
                        "url": page,
                    }),
                );
            } else {
                emit(
                    "info",
                    format!("发现新版本 {tag}（当前 v{current}），可在「检查更新」中获取"),
                );
            }
            return;
        }
        // ---- 桌面端：下载安装包 → 启动安装程序 → 关闭当前程序 ----
        // 新版本：定位安装包资源（历史命名含 `*-setup.exe` 与 `*_x64-setup.exe`
        // 两种形态 → 统一按「以 setup.exe 结尾」匹配，避免漏选）。
        #[cfg(not(target_os = "android"))]
        {
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
        }
    });
}
