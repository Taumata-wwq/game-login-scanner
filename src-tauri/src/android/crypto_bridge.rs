//! Android Keystore 密钥保护桥：Rust 经 JNI 调用 Kotlin `CryptoBridge`。
//!
//! 动机见 [CryptoBridge.kt]：Android 无 DPAPI，敏感字段（access_key/stoken）原先
//! **明文落盘**。改用 AndroidKeyStore 的 AES-256-GCM（密钥不出 TEE）加密。
//!
//! 与 [http_bridge] 同：不依赖 ndk_context，Kotlin object 初始化时 nativeInit 注册类引用。

use std::sync::OnceLock;

use jni::objects::{Global, JClass};
use jni::JavaVM;

/// CryptoBridge 的 JavaVM + 类全局引用。
static BRIDGE: OnceLock<Option<(JavaVM, Global<JClass>)>> = OnceLock::new();

/// Kotlin `CryptoBridge.nativeInit()`：注册 JavaVM 与类全局引用。
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
        .ok_or("密钥桥未初始化（CryptoBridge.nativeInit 未调用）")?;
    vm.attach_current_thread(|env| f(env, gref))
        .map_err(|e| format!("JNI 调用 CryptoBridge 失败: {e:?}"))
}

/// 加密明文，返回 base64(iv||ciphertext)。
pub fn encrypt(plain: &str) -> Result<String, String> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    with_bridge(|env, gref| {
        let jh = env.new_string(plain)?;
        let res = env.call_static_method(
            gref,
            jni_str!("encrypt"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(&jh)],
        )?;
        let obj = res.l()?;
        let jstr = env.cast_local::<jni::objects::JString>(obj)?;
        Ok(jstr.to_string())
    })
}

/// 解密 base64(iv||ciphertext)；失败返回 None。
pub fn decrypt(b64: &str) -> Result<Option<String>, String> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    with_bridge(|env, gref| {
        let jh = env.new_string(b64)?;
        let res = env.call_static_method(
            gref,
            jni_str!("decrypt"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(&jh)],
        )?;
        let obj = res.l()?;
        if obj.is_null() {
            return Ok(None);
        }
        let jstr = env.cast_local::<jni::objects::JString>(obj)?;
        Ok(Some(jstr.to_string()))
    })
}
