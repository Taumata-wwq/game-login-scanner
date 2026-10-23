/// 初始化文件日志与 panic 记录。GUI 进程无控制台，闪退 / 异常只有落到文件
/// 才能事后排查（`%APPDATA%\游戏扫码器\game-login-scanner.log`，超过 2MB 轮转为 .old）。
pub(crate) fn init_file_logging() {
    // Android：日志走 logcat（`adb logcat -s GameScanner`），真机联调主要通道；
    // 文件日志依赖 AppConfig::path()（Android 上的落点不保证可用），移动端不启用。
    #[cfg(target_os = "android")]
    {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("GameScanner"),
        );
    }
    // 桌面：文件日志（用户回传用）+ panic 落盘。
    #[cfg(not(target_os = "android"))]
    init_file_logging_desktop();
}

/// 桌面端文件日志（Windows 专用；Android 走 logcat，见 [`init_file_logging`]）。
#[cfg(not(target_os = "android"))]
fn init_file_logging_desktop() {
    // 只在桌面路径需要：放在函数内而不是文件头，否则 Android 构建会报 unused import。
    use crate::config::AppConfig;

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
