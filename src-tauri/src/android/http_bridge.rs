//! 系统 TLS HTTP 桥（Android）：Rust 经 JNI 调用 Kotlin `HttpBridge.request`。
//!
//! 动机见 [HttpBridge.kt]：米哈游 WAF 按 TLS 指纹拒绝 rustls，Android 无 schannel
//! 回退——改用系统 TLS（HttpURLConnection/Conscrypt）代为发起这些域名的请求。
//!
//! 调用发生在 blocking 线程（未 attach JVM），故每次都 `attach_current_thread`。

/// 发起一次系统 TLS 请求。
///
/// 入参/返回同 Kotlin `HttpBridge.request`（返回 JSON 字符串）。
/// 失败（JNI 异常等）返回 Err。
pub fn request(
    method: &str,
    url: &str,
    headers_json: &str,
    body: Option<&str>,
) -> Result<String, String> {
    use jni::objects::{JClass, JObject, JString, JValue};
    use jni::{jni_sig, jni_str};

    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
    vm.attach_current_thread(|env| -> Result<String, jni::errors::Error> {
        // native 线程的 system classloader 找不到 app 包下的类，必须经 Context
        // 的 ClassLoader 显式加载（JNI 经典陷阱）。
        let context = unsafe { JObject::from_raw(env, ctx.context() as jni::sys::jobject) };
        let loader = env
            .call_method(
                &context,
                jni_str!("getClassLoader"),
                jni_sig!("()Ljava/lang/ClassLoader;"),
                &[],
            )?
            .l()?;
        let cls_name = env.new_string("com.taumata.scanner.plugins.HttpBridge")?;
        let cls_obj = env
            .call_method(
                &loader,
                jni_str!("loadClass"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
                &[JValue::Object(&cls_name)],
            )?
            .l()?;
        let cls = env.cast_local::<JClass>(cls_obj)?;
        let jm = env.new_string(method)?;
        let ju = env.new_string(url)?;
        let jh = env.new_string(headers_json)?;
        let jb = match body {
            Some(b) => JObject::from(env.new_string(b)?),
            None => JObject::null(),
        };
        let res = env.call_static_method(
            &cls,
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
        let jstr = env.cast_local::<JString>(obj)?;
        Ok(jstr.to_string())
    })
    .map_err(|e| format!("JNI 调用 HttpBridge 失败: {e:?}"))
}

/// 解析 Kotlin 返回的 JSON，返回 (status, headers, body)。
/// 失败字段（error）转为 Err。
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
        return Err(format!("系统 TLS 请求失败: {err}"));
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
