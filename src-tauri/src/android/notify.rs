//! Android 通知桥（对位 clipboard.rs 的 JNI 桥写法）。
//!
//! Rust 经 JNI 调用 Kotlin `NotifyBridge`：Kotlin 侧由
//! [crate::screen_capture_plugin] 注册时注入 Context 并触发 nativeInit（注册 JavaVM
//! 与类全局引用）——不依赖 ndk_context（该版本 tao/tauri 不会初始化它）。
//!
//! 用途（手机切到游戏里扫码时界面不可见，通知栏是唯一状态出口）：
//! - 常驻：`show_persistent` / `hide_persistent`（后台扫码状态，回前台撤下）；
//! - 即时：`notify_once` / `cancel`（扫码成功、下播等一次性提醒，可划掉）。
//!
//! 所有调用在非 Android 平台是空实现，便于跨平台编译。

use std::sync::OnceLock;

#[cfg(target_os = "android")]
use jni::objects::{Global, JClass};
#[cfg(target_os = "android")]
use jni::JavaVM;

#[cfg(target_os = "android")]
static BRIDGE: OnceLock<Option<(JavaVM, Global<JClass>)>> = OnceLock::new();

/// Kotlin `NotifyBridge.nativeInit()`：注册 JavaVM 与类全局引用。
#[cfg(target_os = "android")]
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

/// 桥是否已注册（未注册时所有通知调用静默降级，不报错）。
#[cfg(target_os = "android")]
fn ready() -> bool {
    BRIDGE.get().and_then(|o| o.as_ref()).is_some()
}

#[cfg(not(target_os = "android"))]
fn ready() -> bool {
    false
}

/// 显示/更新常驻通知。未初始化时静默忽略（通知不是关键路径，不该因此报错）。
///
/// `target`：点击后要定位的目标（`live:<platform>:<room>`；None/空 → 只回主界面）。
#[cfg(target_os = "android")]
pub fn show_persistent(title: &str, text: &str, target: Option<&str>) {
    if !ready() {
        return;
    }
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};
    let Some((vm, gref)) = BRIDGE.get().and_then(|o| o.as_ref()) else {
        return;
    };
    let _ = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let jt = env.new_string(title)?;
        let jx = env.new_string(text)?;
        // Kotlin 侧签名是 (String, String, String?)：无目标时传 null。
        let jg = match target.filter(|s| !s.is_empty()) {
            Some(t) => JObject::from(env.new_string(t)?),
            None => JObject::null(),
        };
        env.call_static_method(
            &**gref,
            jni_str!("showPersistent"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"),
            &[
                JValue::Object(&jt),
                JValue::Object(&jx),
                JValue::Object(&jg),
            ],
        )?;
        Ok(())
    });
}

#[cfg(not(target_os = "android"))]
pub fn show_persistent(_title: &str, _text: &str, _target: Option<&str>) {}

/// 撤下常驻通知（回到前台 / 关掉开关时调用）。
#[cfg(target_os = "android")]
pub fn hide_persistent() {
    if !ready() {
        return;
    }
    use jni::{jni_sig, jni_str};
    let Some((vm, gref)) = BRIDGE.get().and_then(|o| o.as_ref()) else {
        return;
    };
    let _ = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        env.call_static_method(&**gref, jni_str!("hidePersistent"), jni_sig!("()V"), &[])?;
        Ok(())
    });
}

#[cfg(not(target_os = "android"))]
pub fn hide_persistent() {}

/// 发一条可划掉的即时通知（同一 id 覆盖上一条，避免刷屏）。
///
/// `target`：点击后要定位的目标（`live:<platform>:<room>`；None/空 → 只回主界面）。
#[cfg(target_os = "android")]
pub fn notify_once(id: i32, title: &str, text: &str, target: Option<&str>) {
    if !ready() {
        return;
    }
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};
    let Some((vm, gref)) = BRIDGE.get().and_then(|o| o.as_ref()) else {
        return;
    };
    let _ = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let jt = env.new_string(title)?;
        let jx = env.new_string(text)?;
        let jg = match target.filter(|s| !s.is_empty()) {
            Some(t) => JObject::from(env.new_string(t)?),
            None => JObject::null(),
        };
        env.call_static_method(
            &**gref,
            jni_str!("notifyOnce"),
            jni_sig!("(ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"),
            &[
                JValue::Int(id),
                JValue::Object(&jt),
                JValue::Object(&jx),
                JValue::Object(&jg),
            ],
        )?;
        Ok(())
    });
}

#[cfg(not(target_os = "android"))]
pub fn notify_once(_id: i32, _title: &str, _text: &str, _target: Option<&str>) {}

/// 清除即时通知（按 id）。
#[cfg(target_os = "android")]
pub fn cancel(id: i32) {
    if !ready() {
        return;
    }
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};
    let Some((vm, gref)) = BRIDGE.get().and_then(|o| o.as_ref()) else {
        return;
    };
    let _ = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        env.call_static_method(
            &**gref,
            jni_str!("cancel"),
            jni_sig!("(I)V"),
            &[JValue::Int(id)],
        )?;
        Ok(())
    });
}

#[cfg(not(target_os = "android"))]
pub fn cancel(_id: i32) {}

/// 即时通知的固定 id（沿用语义分组：同一类事件覆盖而非堆积）。
pub mod ids {
    /// 扫码成功。
    pub const SCAN_SUCCESS: i32 = 10031;
    /// 直播间开播/下播等状态变化。
    pub const LIVE_STATUS: i32 = 10032;
    /// 扫码失败 / 会话异常。
    pub const SCAN_ERROR: i32 = 10033;
}
