//! 共享 HTTP 客户端：ureq（阻塞式，经 `spawn_blocking` 包装为 async），进程内
//! 复用连接池与 TLS 会话。
//!
//! 选型说明：各登录端点调用面很窄（POST/GET + 头 + 超时 + 读全文），ureq +
//! 系统 schannel（native-tls）比 reqwest+hyper 全家桶小 ~1.5MB。阻塞调用运行在
//! tokio blocking 线程池，不占用异步 worker；停止扫描时 `select!` 丢弃 future，
//! 底层请求线程随超时自行结束。

use std::sync::OnceLock;
use std::time::Duration;

/// 默认 User-Agent：仅对未显式设置 UA 的请求生效（reqwest 默认不发送 UA，
/// ureq 默认发 `ureq/x.y`——统一改为对各端点都已被社区实现验证过的短 UA）。
const DEFAULT_UA: &str = "Mozilla/5.0 BH3Scanner";

static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

fn agent() -> &'static ureq::Agent {
    AGENT.get_or_init(|| {
        // 未启用 ureq 默认的 rustls（`tls` 特性），显式注入系统 schannel 后端。
        let connector = std::sync::Arc::new(
            native_tls::TlsConnector::builder()
                .build()
                .expect("TLS 初始化失败"),
        );
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .tls_connector(connector)
            .build()
    })
}

/// 请求头列表（键大小写保留；查找时忽略大小写）。
pub type Headers = Vec<(String, String)>;

/// 头部值 ASCII 化：非 ASCII 字节按 UTF-8 百分号编码（ureq 只接受 ASCII 头值，
/// reqwest 曾允许直接发 UTF-8 头）。米哈游 x-rpc 头参考协议本身即用百分号编码
/// （如 `Chrome%20152.0.0.0`），服务端可正常处理；已有 `%` 原样保留防二次编码。
pub fn header_utf8(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii() && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// HTTP 响应（body 已完整读入内存）。
pub struct Response {
    pub status: u16,
    pub headers: Headers,
    pub body: String,
}

impl Response {
    /// 第一个匹配的响应头（忽略大小写）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 全部同名响应头（忽略大小写；Set-Cookie 等重复头用）。
    pub fn headers_all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

fn convert(resp: ureq::Response) -> Result<Response, String> {
    let status = resp.status();
    let headers: Headers = resp
        .headers_names()
        .into_iter()
        .flat_map(|name| {
            resp.all(&name)
                .into_iter()
                .map(move |v| (name.clone(), v.to_string()))
        })
        .collect();
    let body = resp
        .into_string()
        .map_err(|e| format!("读取响应失败: {e}"))?;
    Ok(Response {
        status,
        headers,
        body,
    })
}

async fn request(
    is_post: bool,
    url: String,
    mut headers: Headers,
    body: Option<String>,
) -> Result<Response, String> {
    let agent = agent().clone();
    tokio::task::spawn_blocking(move || {
        let mut req = if is_post {
            agent.post(&url)
        } else {
            agent.get(&url)
        };
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
        {
            headers.push(("User-Agent".to_string(), DEFAULT_UA.to_string()));
        }
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        // POST 用 send_bytes（ureq 只对 send_string 自动补 text/plain 头），
        // 头部与调用方声明保持一一对应。
        let result = if is_post {
            req.send_bytes(body.as_deref().unwrap_or("").as_bytes())
        } else {
            req.call()
        };
        match result {
            Ok(resp) => convert(resp),
            // 4xx/5xx 也读出 body：鹰角 400 带 msg、风控页等调用方需要内容。
            Err(ureq::Error::Status(_, resp)) => convert(resp),
            Err(e) => Err(format!("{e}")),
        }
    })
    .await
    .map_err(|e| format!("HTTP 任务失败: {e}"))?
}

/// GET 请求。
pub async fn get(url: &str, headers: Headers) -> Result<Response, String> {
    request(false, url.to_string(), headers, None).await
}

/// POST 请求（body 为原始字符串）。
pub async fn post(url: &str, headers: Headers, body: &str) -> Result<Response, String> {
    request(true, url.to_string(), headers, Some(body.to_string())).await
}
