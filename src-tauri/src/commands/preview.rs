use crate::prelude::*;

use std::sync::Mutex;

/// 预览 GDI 兜底缓存：桌面复制不可用时降级（内部限频 60fps），
/// 保存最近一次负载（前 24 字节为头部，用于「帧未变」的头部专用响应）。
struct GdiPreviewCache {
    at: std::time::Instant,
    payload: Vec<u8>,
}

static GDI_PREVIEW: Mutex<Option<GdiPreviewCache>> = Mutex::new(None);

/// 预览数据帧发送节流：桌面内容高速变化时上限为 60fps（16ms），避免前端 60Hz
/// 轮询时重复发送同一帧。区域或宽度变化（平移/缩放）时不节流，保证立刻拿到新画面。
/// 预览节流记录：（上次发送时刻， 请求区域， 输出宽度）
type PreviewThrottle = (std::time::Instant, (i32, i32, i32, i32), u32);

static LAST_PREVIEW_DATA: Mutex<Option<PreviewThrottle>> = Mutex::new(None);

/// 桌面预览：返回**当前可视视口**的实时缩略图（前端缩放/平移后只取所视区域，
/// 高倍放大时每输出像素仅覆盖少量屏幕像素 → 清晰），供前端当「监视器」展示。
///
/// 数据源优先走 DXGI 桌面复制（静止画面近零开销），不可用时回退 GDI（限频）。
///
/// 返回原始二进制（`tauri::ipc::Response`，前端收到 ArrayBuffer）：
/// 24 字节小端头 + RGBA 数据，头部布局
/// `width:u32, height:u32, mon.x:i32, mon.y:i32, mon.w:i32, mon.h:i32`。
/// **仅有 24 字节头部**表示「画面未变化」，前端直接沿用上一帧。
#[tauri::command]
pub(crate) async fn capture_preview(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    max_width: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    // 取帧 / BGRA→RGB / 节流全程同步且可能耗时（DXGI Map 数毫秒），放到 blocking
    // 线程池执行，避免占住 tokio async 工作线程（P1-16）。
    tauri::async_runtime::spawn_blocking(move || capture_preview_sync(&app, x, y, w, h, max_width))
        .await
        .map_err(|e| format!("预览任务失败: {e}"))?
}

/// 「显示器列表为空」是否属于正常瞬态。
///
/// Android 上屏幕尺寸要等 MediaProjection 授权成功、`ScreenCaptureBridge.onScreenInfo`
/// 上报之后才可知，而预览轮询在应用启动瞬间就开始跑 → 首帧前 `screen_rects()`
/// 必然为空。把它当错误会让每次冷启动都在日志里留一条假的
/// 「预览失败: 未找到可用显示器」（真机实测）。桌面端空列表则是真实故障。
#[cfg(target_os = "android")]
const DISPLAY_PENDING_IS_BENIGN: bool = true;

#[cfg(not(target_os = "android"))]
const DISPLAY_PENDING_IS_BENIGN: bool = false;

/// `capture_preview` 的同步实现（在 blocking 池执行）。
fn capture_preview_sync(
    app: &tauri::AppHandle,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    max_width: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    // 用带 1 秒缓存的 screen_rects() 枚举显示器（每帧实时枚举是预览帧率上不去的瓶颈之一）。
    let screens = crate::screenshot::screen_rects();
    if screens.is_empty() {
        // 移动端：屏幕尺寸尚未上报（未授权 / 首帧未到）→ 返回「无变化」空帧，
        // 前端静默跳过本帧，等下一次轮询。
        if DISPLAY_PENDING_IS_BENIGN {
            return Ok(tauri::ipc::Response::new(vec![0u8; 24]));
        }
        return Err("未找到可用显示器".to_string());
    }
    // 主窗口中心点所在显示器（找不到则第一块屏）；预览始终取该屏，
    // 区域越界部分由 duplication 裁剪夹紧。
    let center = app
        .get_webview_window("main")
        .and_then(|win| {
            let pos = win.outer_position().ok()?;
            let size = win.outer_size().ok()?;
            Some((
                pos.x + size.width as i32 / 2,
                pos.y + size.height as i32 / 2,
            ))
        })
        .unwrap_or((0, 0));
    let d = screens
        .iter()
        .find(|(x, y, w, h)| {
            center.0 >= *x && center.0 < *x + *w && center.1 >= *y && center.1 < *y + *h
        })
        .copied()
        .unwrap_or(screens[0]);
    let (dx, dy, mw, mh) = d;
    let want_w = max_width.unwrap_or(384).clamp(80, 4096);
    // 注：实际输出宽度由 duplication::preview_frame 进一步钳制到「不超过裁剪区
    // 实际像素数」（禁止上采样），避免放大时出现黑色网格线。
    // 区域非法（未初始化）时回退整屏。
    let (rx, ry, rw, rh) = if w > 0 && h > 0 {
        (x, y, w, h)
    } else {
        (dx, dy, mw, mh)
    };

    match crate::duplication::preview_frame((rx, ry, rw, rh), want_w) {
        Ok(crate::duplication::PreviewOut::Frame(rgba, tw, th)) => {
            // 数据帧节流到 60fps：区域与宽度都没变、距上次发送 < 16ms 时沿用旧帧。
            let now = std::time::Instant::now();
            let mut last = LAST_PREVIEW_DATA
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let throttled = matches!(
                last.as_ref(),
                Some((t, k, tw2))
                if now.duration_since(*t).as_millis() < 16 && *k == (rx, ry, rw, rh) && *tw2 == tw
            );
            if throttled {
                return Ok(tauri::ipc::Response::new(vec![0u8; 24]));
            }
            *last = Some((now, (rx, ry, rw, rh), tw));
            Ok(tauri::ipc::Response::new(preview_payload(
                tw, th, dx, dy, mw, mh, rgba,
            )))
        }
        Ok(crate::duplication::PreviewOut::Unchanged) => {
            // 24 字节头部专用响应（全零）：前端据此跳过重绘。
            Ok(tauri::ipc::Response::new(vec![0u8; 24]))
        }
        Err(_) => {
            // GDI 兜底：限频 60fps（16ms），未到间隔时沿用缓存帧。
            let mut g = GDI_PREVIEW
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(c) = g.as_ref() {
                if c.at.elapsed() < std::time::Duration::from_millis(16) {
                    return Ok(tauri::ipc::Response::new(c.payload[..24].to_vec()));
                }
            }
            let frame = crate::screenshot::capture_region(rx, ry, rw, rh).ok_or("截取屏幕失败")?;
            let tw = frame.w.clamp(1, want_w);
            let th = ((frame.h as u64 * tw as u64) / frame.w as u64).max(1) as u32;
            let mut rgba = Vec::with_capacity(tw as usize * th as usize * 4);
            for ty in 0..th {
                let sy = (ty * frame.h) / th;
                for tx in 0..tw {
                    let sx = (tx * frame.w) / tw;
                    let i = ((sy * frame.w) + sx) as usize * 3;
                    rgba.push(frame.rgb[i]);
                    rgba.push(frame.rgb[i + 1]);
                    rgba.push(frame.rgb[i + 2]);
                    rgba.push(255);
                }
            }
            let payload = preview_payload(tw, th, dx, dy, mw, mh, rgba);
            *g = Some(GdiPreviewCache {
                at: std::time::Instant::now(),
                payload: payload.clone(),
            });
            Ok(tauri::ipc::Response::new(payload))
        }
    }
}

/// 组装预览二进制负载（24 字节头 + RGBA）。
pub(crate) fn preview_payload(
    tw: u32,
    th: u32,
    mx: i32,
    my: i32,
    mw: i32,
    mh: i32,
    rgba: Vec<u8>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(24 + rgba.len());
    out.extend_from_slice(&tw.to_le_bytes());
    out.extend_from_slice(&th.to_le_bytes());
    out.extend_from_slice(&mx.to_le_bytes());
    out.extend_from_slice(&my.to_le_bytes());
    out.extend_from_slice(&mw.to_le_bytes());
    out.extend_from_slice(&mh.to_le_bytes());
    out.extend_from_slice(&rgba);
    out
}

/// 瓦片（拖拽底图）增量帧。开启瓦片模式后，每次返回自上次以来变化的瓦片。
/// 负载：8 字节头（tw,th）+ 每瓦片 16 字节头（col,row,w,h）+ RGBA。
#[tauri::command]
pub(crate) async fn capture_tiles(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    max_width: u32,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || capture_tiles_sync(&app, x, y, w, h, max_width))
        .await
        .map_err(|e| format!("瓦片任务失败: {e}"))?
}

/// `capture_tiles` 的同步实现（在阻塞池执行；拖拽时 30Hz 调用，含整屏降采样）。
fn capture_tiles_sync(
    app: &tauri::AppHandle,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    max_width: u32,
) -> Result<tauri::ipc::Response, String> {
    let screens = crate::screenshot::screen_rects();
    if screens.is_empty() {
        // 同 capture_preview_sync：移动端未授权/首帧前属正常瞬态，返回「无瓦片」。
        if DISPLAY_PENDING_IS_BENIGN {
            return Ok(tauri::ipc::Response::new(vec![0u8; 8]));
        }
        return Err("未找到可用显示器".to_string());
    }
    let center = app
        .get_webview_window("main")
        .and_then(|win| {
            let pos = win.outer_position().ok()?;
            let size = win.outer_size().ok()?;
            Some((
                pos.x + size.width as i32 / 2,
                pos.y + size.height as i32 / 2,
            ))
        })
        .unwrap_or((0, 0));
    let d = screens
        .iter()
        .find(|(sx, sy, sw, sh)| {
            center.0 >= *sx && center.0 < *sx + *sw && center.1 >= *sy && center.1 < *sy + *sh
        })
        .copied()
        .unwrap_or(screens[0]);
    let (dx, dy, mw, mh) = d;
    let (rx, ry, rw, rh) = if w > 0 && h > 0 {
        (x, y, w, h)
    } else {
        (dx, dy, mw, mh)
    };
    let want_w = max_width.clamp(80, 4096);
    match crate::duplication::tile_frame((rx, ry, rw, rh), want_w) {
        Ok(Some((tw, th, tiles))) => {
            let mut out = Vec::with_capacity(8 + tiles.len() * 16);
            out.extend_from_slice(&tw.to_le_bytes());
            out.extend_from_slice(&th.to_le_bytes());
            for (c, r, tw2, th2, rgba) in tiles {
                out.extend_from_slice(&c.to_le_bytes());
                out.extend_from_slice(&r.to_le_bytes());
                out.extend_from_slice(&tw2.to_le_bytes());
                out.extend_from_slice(&th2.to_le_bytes());
                out.extend_from_slice(&rgba);
            }
            Ok(tauri::ipc::Response::new(out))
        }
        // 无变化：仅 8 字节头。
        Ok(None) => Ok(tauri::ipc::Response::new(vec![0u8; 8])),
        Err(_) => Err("桌面复制不可用".to_string()),
    }
}

/// 开启/关闭瓦片（拖拽底图）模式。
#[tauri::command]
pub(crate) fn set_tile_mode(x: i32, y: i32, w: i32, h: i32, on: bool) {
    crate::duplication::set_tile_mode((x, y, w, h), on);
}

/// 暂停/恢复预览：暂停时清除后端预览关注区，让帧总线跳过拷贝（避免暂停态 GPU 升高）。
#[tauri::command]
pub(crate) fn set_preview_active(active: bool) {
    if !active {
        crate::duplication::clear_preview_roi();
    }
}

// ---------- 直播流（B站低延迟捕获） ----------
