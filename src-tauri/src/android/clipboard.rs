//! Android 剪贴板桥与门面（对位 Windows clipboard.rs）。
//!
//! Rust 经 JNI 调用 Kotlin `ClipboardBridge.readImage()`：Kotlin 侧由
//! [crate::screen_capture_plugin] 注册时注入 Context 并触发 nativeInit（注册
//! JavaVM 与类全局引用）——不依赖 ndk_context（该版本 tao/tauri 不会初始化它）。
//!
//! 数据面：`[w:u32 LE][h:u32 LE][RGBA...]`（单次低频传输，非热路径；解析见
//! [crate::qr::parse_clipboard_payload]，纯函数有单测覆盖）。
//!
//! 平台限制：Android 10+ 后台应用读剪贴板受限（返回 null），前台（如摄像头
//! 取景启动时）可用；Android 9 无限制。受限场景由屏幕 / 摄像头路径兜底。

use std::sync::OnceLock;

use jni::objects::{Global, JClass};
use jni::JavaVM;

/// ClipboardBridge 的 JavaVM + 类全局引用（attach 时注册，进程内单例）。
static BRIDGE: OnceLock<Option<(JavaVM, Global<JClass>)>> = OnceLock::new();

/// Kotlin `ClipboardBridge.nativeInit()`：注册 JavaVM 与类全局引用。
pub fn init_from_env(env: &mut jni::Env, this: &jni::objects::JObject) {
    let vm = match env.get_java_vm() {
        Ok(v) => v,
        Err(_) => return,
    };
    let class = match env.get_object_class(this) {
        Ok(c) => c,
        Err(_) => return,
    };
    let gref = match env.new_global_ref(class) {
        Ok(g) => g,
        Err(_) => return,
    };
    let _ = BRIDGE.set(Some((vm, gref)));
}

fn with_bridge<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&mut jni::Env, &JClass) -> Result<T, jni::errors::Error>,
{
    let (vm, gref) = BRIDGE
        .get()
        .and_then(|o| o.as_ref())
        .ok_or("剪贴板桥未初始化（ClipboardBridge.attach 未调用）")?;
    vm.attach_current_thread(|env| f(env, gref))
        .map_err(|e| format!("JNI 调用 ClipboardBridge 失败: {e:?}"))
}

/// 读取剪贴板位图，返回 (RGBA, 宽, 高)。无图 / 异常 / 未初始化均返回 None。
pub fn read_image_rgba() -> Option<(Vec<u8>, u32, u32)> {
    use jni::{jni_sig, jni_str};

    let payload = with_bridge(|env, gref| {
        let res = env.call_static_method(gref, jni_str!("readImage"), jni_sig!("()[B"), &[])?;
        let obj = res.l()?;
        if obj.is_null() {
            return Ok(None);
        }
        let arr = env.cast_local::<jni::objects::JByteArray>(obj)?;
        Ok(Some(env.convert_byte_array(&arr)?))
    })
    .ok()
    .flatten()?;
    crate::qr::parse_clipboard_payload(&payload)
}
