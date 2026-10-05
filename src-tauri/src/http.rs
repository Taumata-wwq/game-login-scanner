//! 共享 HTTP 客户端：进程内复用连接池与 TLS 会话，避免每请求新建。

use std::sync::OnceLock;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// 全局共享的 reqwest 客户端。
/// 带超时：避免网络异常时请求永久挂起（导致二维码一直"生成中"）。
pub fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("HTTP 客户端初始化失败")
    })
}
