//! Android 平台适配层。
//!
//! 设计原则（见 docs/安卓端开发文档.md）：
//! - Rust 写业务，Kotlin 写平台适配；
//! - 本模块只包含 Android 强绑定的能力：屏幕捕获、摄像头帧源、视频硬解、密钥存储。
//! - 与 Windows 对应模块（duplication / screenshot / mf_decode / clipboard / crypto::dpapi）
//!   一一对位，通过 cfg 分叉或 trait 实现切换。

pub mod camera;
pub mod capture;

/// Android 应用私有文件目录（<filesDir>），用于配置持久化。
///
/// 通过 ndk-context 取 Activity Context → 调用 `getFilesDir()`。
/// 在 `run()` 早期（Tauri app 尚未建立）即可调用。
pub fn files_dir() -> Option<std::path::PathBuf> {
    use jni::objects::{JObject, JString};
    use jni::{jni_sig, jni_str};
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
    vm.attach_current_thread(|env| -> Result<String, jni::errors::Error> {
        let activity = unsafe { JObject::from_raw(env, ctx.context() as jni::sys::jobject) };
        let files = env
            .call_method(
                &activity,
                jni_str!("getFilesDir"),
                jni_sig!("()Ljava/io/File;"),
                &[],
            )?
            .l()?;
        let path = env
            .call_method(
                &files,
                jni_str!("getAbsolutePath"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        let jstr = env.cast_local::<JString>(path)?;
        Ok(jstr.to_string())
    })
    .ok()
    .map(std::path::PathBuf::from)
}
pub mod clipboard;
pub mod duplication;
pub mod jni_bridge;
pub mod mediacodec_decode;
pub mod screenshot;
