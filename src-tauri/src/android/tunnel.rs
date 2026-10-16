//! 系统 TLS 流式隧道（Android）：Rust 经 JNI 调用 Kotlin `StreamTunnel.open`。
//!
//! 动机见 [StreamTunnel.kt]：抖音/米哈游 WAF 拒绝 rustls——API 用 [http_bridge]（缓冲），
//! FLV 流用本隧道（本地监听端口 ↔ 系统 TLS 双向透传）。Rust 侧连本地端口即得透明流。
//!
//! 与 [http_bridge] 同：不依赖 ndk_context，Kotlin object 初始化时 nativeInit 注册类引用。

use std::sync::OnceLock;

use jni::objects::{Global, JClass};
use jni::JavaVM;

/// StreamTunnel 的 JavaVM + 类全局引用。
static BRIDGE: OnceLock<Option<(JavaVM, Global<JClass>)>> = OnceLock::new();

/// Kotlin `StreamTunnel.nativeInit()`：注册 JavaVM 与类全局引用。
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

/// 打开到 target host:port 的系统 TLS 隧道，返回本地监听端口。
pub fn open(host: &str, port: u16) -> Result<u16, String> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    let (vm, gref) = BRIDGE
        .get()
        .and_then(|o| o.as_ref())
        .ok_or("流隧道未初始化（StreamTunnel.nativeInit 未调用）")?;
    vm.attach_current_thread(|env| -> Result<u16, jni::errors::Error> {
        let jh = env.new_string(host)?;
        let res = env.call_static_method(
            &**gref,
            jni_str!("open"),
            jni_sig!("(Ljava/lang/String;I)I"),
            &[JValue::Object(&jh), JValue::Int(port as i32)],
        )?;
        Ok(res.i()? as u16)
    })
    .map_err(|e| format!("JNI 调用 StreamTunnel 失败: {e:?}"))
}
