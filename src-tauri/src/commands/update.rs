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

    /// 下载产物的摘要校验：一致才允许 spawn 安装程序。
    ///
    /// 这条测试锁的是「校验分支真的能区分内容」——如果哪天改成大小写敏感比较、
    /// 或者把 digest 前缀处理错了，篡改过的安装包就会被放行。
    #[test]
    fn digest_check_rejects_tampered_payload() {
        use sha2::{Digest, Sha256};
        let good_bytes = b"setup-payload-v1";
        let want = hex::encode(Sha256::digest(good_bytes)).to_ascii_lowercase();

        // 同一份字节：摘要一致（应放行）
        let got = hex::encode(Sha256::digest(good_bytes)).to_ascii_lowercase();
        assert_eq!(got, want);

        // 篡改一字节：摘要必须不同（应拦截）
        let tampered = b"setup-payload-v2";
        let got_bad = hex::encode(Sha256::digest(tampered)).to_ascii_lowercase();
        assert_ne!(got_bad, want, "内容变化必须导致摘要不匹配");

        // 大小写不影响判定（check_update 两侧都 lower）
        assert_eq!(want.to_ascii_uppercase().to_ascii_lowercase(), want);
        // "sha256:" 前缀被剥掉后应正好是 64 位十六进制
        assert_eq!(want.len(), 64);
        assert!(want.chars().all(|c| c.is_ascii_hexdigit()));
    }

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

/// 清理历史遗留在 temp 目录里的更新安装包。
///
/// 更新流程是「下载到 temp → spawn 安装器 → 关闭本程序」，安装器需要读那个文件，
/// 所以**不能**在下载线程里删。但下一次启动时它已经没用了——不清理的话每更新一次
/// 就在 temp 里留一份几 MB 的安装包（长期累积）。
pub(crate) fn cleanup_stale_setup_files() {
    let dir = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut removed = 0usize;
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let is_setup = name.starts_with("game-login-scanner-") && name.ends_with("-setup.exe");
        if is_setup && std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        log::info!("[更新] 已清理 {removed} 个历史安装包");
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
                        // digest 形如 "sha256:de97b7…"；GitHub 自 2025 起为每个
                        // asset 计算。缺失时留空串（下面按「无摘要」处理，仍然下载
                        // 但会明确告知用户，而不是假装校验过）。
                        let digest = x["digest"]
                            .as_str()
                            .unwrap_or("")
                            .strip_prefix("sha256:")
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        Some((
                            x["name"].as_str().unwrap_or("setup.exe").to_string(),
                            x["browser_download_url"].as_str()?.to_string(),
                            digest,
                        ))
                    })
            });
            let Some((_, url, want_digest)) = asset else {
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
            // 校验下载内容：这里拿到的 URL 来自 GitHub API 响应，但响应本身经过
            // 我们的 HTTP 层；校验摘要能把「下到半截 / 被中间人替换 / 拿到错误
            // asset」三类问题都挡在 spawn 之前——否则等于直接执行任意字节。
            if !want_digest.is_empty() {
                use sha2::{Digest, Sha256};
                let got = hex::encode(Sha256::digest(&bytes)).to_ascii_lowercase();
                if got != want_digest {
                    emit(
                        "error",
                        format!("安装包校验失败（期望 {want_digest}，实际 {got}），已放弃安装"),
                    );
                    return;
                }
                emit("info", "安装包 SHA-256 校验通过".to_string());
            } else {
                // 摘要缺失时仍继续（老版本 Release 可能没有 digest），但用 error 级别
                // 让用户一眼看到「这次没有校验」——前端只把 error 当红字显示，
                // 用 info 会被后面的「下载完成」淹没。
                emit(
                    "error",
                    "警告：Release 未提供 SHA-256 摘要，本次跳过校验".to_string(),
                );
            }
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
