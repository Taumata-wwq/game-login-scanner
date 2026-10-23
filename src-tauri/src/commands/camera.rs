use crate::prelude::*;

/// 相机预览帧（Android）：24 字节头（w,h,0,0,w=相机宽,h=相机高）+ RGBA。
/// 无新帧返回 24 字节零头（前端沿用上一帧）；相机未运行返回空。
#[tauri::command]
pub(crate) async fn capture_camera_frame(
    max_width: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mw = max_width.unwrap_or(480).clamp(80, 4096);
        match crate::camera_plugin::camera_preview(mw) {
            Some((rgba, tw, th)) => Ok(tauri::ipc::Response::new(preview_payload(
                tw, th, 0, 0, tw as i32, th as i32, rgba,
            ))),
            None => Ok(tauri::ipc::Response::new(vec![0u8; 24])),
        }
    })
    .await
    .map_err(|e| format!("相机取帧任务失败: {e}"))?
}
