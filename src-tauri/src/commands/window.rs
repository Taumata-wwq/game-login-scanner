use std::sync::Mutex;

use crate::prelude::*;

use crate::AppState;

/// 应用窗口置顶设置（桌面特性；Android 无此概念，空实现）。
#[cfg(not(target_os = "android"))]
pub(crate) fn apply_always_on_top(app: &tauri::AppHandle, on_top: bool) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_always_on_top(on_top);
    }
}

#[cfg(target_os = "android")]
pub(crate) fn apply_always_on_top(_app: &tauri::AppHandle, _on_top: bool) {}

/// 记住主窗口当前坐标与尺寸（关闭 / 移动 / 缩放后调用，供下次启动恢复）。
///
/// **最小化时跳过**：最小化会把窗口挪到 (-32000,-32000) 并缩到 160×28，
/// 若照存，下次启动会恢复成屏幕外的迷你窗口（实测踩中）。
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
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
}

/// 防抖版窗口几何保存：拖动/缩放事件密集，最多每 800ms 落盘一次；
/// 事件停止后再**尾部补存**一次，避免「最后一次事件的值」丢失
/// （实测：无尾部补偿时拉伸后的最终尺寸只存到中途值）。
pub(crate) fn save_window_geometry_throttled(app: &tauri::AppHandle) {
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
