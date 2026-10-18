//! 系统 TLS HTTP 桥（Android）：Rust 经 JNI 调用 Kotlin `HttpBridge.request`。
//!
//! 动机见 [HttpBridge.kt]：米哈游 WAF 按 TLS 指纹拒绝 rustls，Android 无 schannel
//! 回退——改用系统 TLS（HttpURLConnection/Conscrypt）代为发起这些域名的请求。
//!
//! **不依赖 ndk_context**：该版本 tao/tauri 不会调用 `initialize_android_context`，
//! `android_context()` 会直接 panic（真机实测）。改为 Kotlin `HttpBridge` 在 object
//! 初始化时调用 [init_from_env]（JNI `nativeInit`）主动注册 JavaVM 与本对象引用。

use std::sync::OnceLock;

use jni::objects::{Global, JClass};
use jni::JavaVM;

/// HttpBridge 的 JavaVM + **类**全局引用（Kotlin 初始化时注册，进程内单例）。
///
/// 用类引用 + `call_static_method`：Kotlin `@JvmStatic fun request` 生成的是静态方法。
static BRIDGE: OnceLock<Option<(JavaVM, Global<JClass>)>> = OnceLock::new();

/// Kotlin `HttpBridge.nativeInit()`：注册 JavaVM 与类全局引用。
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

/// 发起一次系统 TLS 请求（调用 Kotlin `HttpBridge.request`）。
pub fn request(
    method: &str,
    url: &str,
    headers_json: &str,
    body: Option<&str>,
) -> Result<String, String> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    let (vm, gref) = BRIDGE
        .get()
        .and_then(|o| o.as_ref())
        .ok_or("系统 TLS 桥未初始化（HttpBridge.nativeInit 未调用）")?;
    vm.attach_current_thread(|env| -> Result<String, jni::errors::Error> {
        use jni::objects::JObject;
        let jm = env.new_string(method)?;
        let ju = env.new_string(url)?;
        let jh = env.new_string(headers_json)?;
        let jb = match body {
            Some(b) => JObject::from(env.new_string(b)?),
            None => JObject::null(),
        };
        let res = env.call_static_method(
            &**gref,
            jni_str!("request"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"),
            &[
                JValue::Object(&jm),
                JValue::Object(&ju),
                JValue::Object(&jh),
                JValue::Object(&jb),
            ],
        )?;
        let obj = res.l()?;
        let jstr = env.cast_local::<jni::objects::JString>(obj)?;
        Ok(jstr.to_string())
    })
    .map_err(|e| format!("JNI 调用 HttpBridge 失败: {e:?}"))
}

/// 解析 Kotlin 返回的 JSON，返回 (status, headers, body)。
pub fn request_bytes(
    method: &str,
    url: &str,
    headers_json: &str,
    body: Option<&str>,
) -> Result<(u16, crate::http::Headers, Vec<u8>), String> {
    use base64::Engine as _;
    let json = request(method, url, headers_json, body)?;
    let v: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("HttpBridge 返回非 JSON: {e}"))?;
    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
        // 带上 URL：同一句话可能来自米哈游 / 抖音 / B站游戏 SDK 等多个域名，
        // 没有 URL 就无法判断是哪条链路失败（真机排障必需）。
        return Err(format!("系统 TLS 请求失败: {err}（{method} {url}）"));
    }
    let status = v.get("status").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
    let mut headers = Vec::new();
    if let Some(h) = v.get("headers").and_then(|x| x.as_object()) {
        for (k, val) in h {
            if let Some(s) = val.as_str() {
                headers.push((k.clone(), s.to_string()));
            }
        }
    }
    let b64 = v.get("bodyBase64").and_then(|x| x.as_str()).unwrap_or("");
    let body = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("HttpBridge body 解码失败: {e}"))?;
    Ok((status, headers, body))
}
