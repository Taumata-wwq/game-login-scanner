//! 渠道登录移动插件装配（Android 真实现；其他平台空装配）。
//!
//! 与 Kotlin 侧 `com.taumata.scanner.plugins.ChannelLoginPlugin` 对应；对位参考项目
//! `login/LoginInstanceManager` 的「按 server_type 分发」——本模块为单一入口，Kotlin
//! 侧内部按 channel 分发到各厂商 SDK（首批小米）。
//! 设计说明同 [`crate::camera_plugin`]：本模块跨平台编译。

#[cfg(target_os = "android")]
use std::sync::OnceLock;
#[cfg(target_os = "android")]
use tauri::plugin::PluginHandle;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::Wry;

#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "com.taumata.scanner.plugins";
#[cfg(target_os = "android")]
const PLUGIN_CLASS: &str = "ChannelLoginPlugin";

#[cfg(target_os = "android")]
static HANDLE: OnceLock<PluginHandle<Wry>> = OnceLock::new();

/// 注册渠道登录插件。
pub fn init() -> TauriPlugin<Wry> {
    Builder::<Wry, ()>::new("channel-login")
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

/// 调起指定渠道的登录 UI，返回 `(凭证 JSON, 渠道 UID, 昵称)`。
///
/// Android：Kotlin SDK 弹登录 UI（阻塞至用户完成 / 取消）；其他平台返回错误。
pub async fn login(channel: &str) -> Result<(String, String, String), String> {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Deserialize)]
        struct Out {
            cred: String,
            #[serde(default)]
            uid: String,
            #[serde(default)]
            name: String,
        }
        let h = HANDLE.get().ok_or("渠道登录插件未注册")?;
        let args = serde_json::json!({ "channel": channel });
        let out: Out = h
            .run_mobile_plugin_async("login", args)
            .await
            .map_err(|e| format!("渠道登录失败：{e}"))?;
        Ok((out.cred, out.uid, out.name))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = channel;
        Err("当前平台不支持渠道登录".to_string())
    }
}
