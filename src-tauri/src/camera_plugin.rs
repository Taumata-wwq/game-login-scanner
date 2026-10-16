//! 摄像头扫码移动插件装配（Android 真实现；其他平台空装配）。
//!
//! 与 Kotlin 侧 `com.taumata.scanner.plugins.CameraCapturePlugin` 对应。
//! 设计说明同 [`crate::screen_capture_plugin`]：本模块跨平台编译。

#[cfg(target_os = "android")]
use std::sync::OnceLock;
#[cfg(target_os = "android")]
use tauri::plugin::PluginHandle;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::Wry;

#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "com.taumata.scanner.plugins";
#[cfg(target_os = "android")]
const PLUGIN_CLASS: &str = "CameraCapturePlugin";

#[cfg(target_os = "android")]
static HANDLE: OnceLock<PluginHandle<Wry>> = OnceLock::new();

/// 注册摄像头扫码插件。
pub fn init() -> TauriPlugin<Wry> {
    Builder::<Wry, ()>::new("camera-capture")
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

#[cfg(target_os = "android")]
pub fn handle() -> Option<&'static PluginHandle<Wry>> {
    HANDLE.get()
}

/// 请求相机权限（Android）：拉起运行时权限对话框。
#[tauri::command]
pub async fn request_camera_permission() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let h = handle().ok_or("相机插件未注册")?;
        h.run_mobile_plugin_async::<serde_json::Value>("requestCamera", ())
            .await
            .map(|_| ())
            .map_err(|e| format!("请求相机权限失败：{e}"))
    }
    #[cfg(not(target_os = "android"))]
    {
        Err("当前平台不支持相机".to_string())
    }
}

/// 启动摄像头捕获（要求已授权）。
#[tauri::command]
pub async fn start_camera_capture() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let h = handle().ok_or("相机插件未注册")?;
        h.run_mobile_plugin_async::<serde_json::Value>("startCapture", ())
            .await
            .map(|_| ())
            .map_err(|e| format!("启动相机失败：{e}"))
    }
    #[cfg(not(target_os = "android"))]
    {
        Err("当前平台不支持相机".to_string())
    }
}

/// 停止摄像头捕获；其他平台空操作成功。
#[tauri::command]
pub async fn stop_camera_capture() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let h = handle().ok_or("相机插件未注册")?;
        h.run_mobile_plugin_async::<serde_json::Value>("stopCapture", ())
            .await
            .map(|_| ())
            .map_err(|e| format!("停止相机失败：{e}"))
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(())
    }
}

/// 查询摄像头捕获状态；其他平台恒 false。
#[tauri::command]
pub async fn camera_capture_state() -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Deserialize)]
        struct State {
            capturing: bool,
        }
        let h = handle().ok_or("相机插件未注册")?;
        let s: State = h
            .run_mobile_plugin_async("captureState", ())
            .await
            .map_err(|e| format!("查询相机状态失败：{e}"))?;
        Ok(s.capturing)
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(false)
    }
}

/// 相机预览帧（Android）：返回 RGBA + 宽高；无新帧返回 None；不可用返回 Err。
pub fn camera_preview(tw: u32) -> Option<(Vec<u8>, u32, u32)> {
    #[cfg(target_os = "android")]
    {
        crate::android::camera::preview_frame(tw)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = tw;
        None
    }
}

/// 相机 sensor 方向（度；预览旋转用）。
pub fn camera_rotation() -> u32 {
    #[cfg(target_os = "android")]
    {
        crate::android::camera::rotation()
    }
    #[cfg(not(target_os = "android"))]
    {
        0
    }
}

/// 相机扫码取帧（Android）：返回灰度帧；无新帧 None。
pub fn camera_scan_frame(scale: u32) -> Option<crate::screenshot::GrayFrame> {
    #[cfg(target_os = "android")]
    {
        crate::android::camera::scan_frame(scale)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = scale;
        None
    }
}
