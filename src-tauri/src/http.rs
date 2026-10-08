//! 共享 HTTP 客户端：手写 HTTP/1.1 + rustls（经 `spawn_blocking` 包装为 async）。
//!
//! 为什么不用 ureq：本工具的 HTTP 调用面极窄（GET/POST + 头 + JSON 响应），
//! ureq 会拖入 `url` → `idna` → `icu_*` 全套 Unicode 表（数百 KB）。而直播拉流
//! 已手写 rustls + HTTP/1.1 管道（schannel 对 B站 CDN 长连接读乱码，故用纯 Rust
//! TLS）——把同一管道泛化为请求/响应客户端，零额外依赖。API 调用低频
//! （登录/轮询），每请求新建连接的握手开销可忽略。阻塞调用运行在 tokio
//! blocking 线程池；停止扫描时 `select!` 丢弃 future，底层线程随超时自行结束。

use std::io::{Read, Write};
use std::net::ToSocketAddrs;
use std::time::Duration;

use rustls::pki_types::ServerName;

/// 默认 User-Agent：仅对未显式设置 UA 的请求生效（对各端点都已被社区实现
/// 验证过的短 UA）。
const DEFAULT_UA: &str = "Mozilla/5.0 BH3Scanner";

/// API 请求的连接/读超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// 响应体上限（防异常服务器撑爆内存）。
const MAX_BODY: usize = 8 * 1024 * 1024;
/// 重定向跟随上限。
const MAX_REDIRECTS: usize = 6;

/// 请求头列表（键大小写保留；查找时忽略大小写）。
pub type Headers = Vec<(String, String)>;

/// 头部值 ASCII 化：非 ASCII 字节按 UTF-8 百分号编码（HTTP/1.1 头仅允许
/// 可见 ASCII）。米哈游 x-rpc 头参考协议本身即用百分号编码
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

/// URL 拆分：仅支持 http(s) 绝对地址，返回 (是否 https, host, port, path?query)。
pub(crate) fn split_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("仅支持 http(s) URL: {url}"));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| format!("端口无效: {authority}"))?,
        ),
        None => (authority.to_string(), if https { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("URL 缺少主机名: {url}"));
    }
    Ok((https, host, port, path.to_string()))
}

/// rustls TLS 流（加载系统根证书，ALPN 声明 http/1.1——rustls 默认不发 ALPN
/// 扩展，B站 CDN（openresty）对无 ALPN 的 ClientHello 会拒绝，表现为乱码/EOF）。
pub(crate) fn tls_connect(
    host: &str,
    port: u16,
    read_timeout: Duration,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("解析主机 {host} 失败: {e}"))?
        .next()
        .ok_or_else(|| format!("主机 {host} 无可用地址"))?;
    let tcp = std::net::TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("连接 {host}:{port} 失败: {e}"))?;
    tcp.set_nodelay(true).ok();
    tcp.set_read_timeout(Some(read_timeout))
        .map_err(|e| format!("设置读超时失败: {e}"))?;
    let mut store = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = store.add(cert);
    }
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let conn = rustls::ClientConnection::new(
        std::sync::Arc::new(config),
        ServerName::try_from(host.to_string()).map_err(|_| "服务器名无效".to_string())?,
    )
    .map_err(|e| format!("TLS 初始化失败: {e}"))?;
    Ok(rustls::StreamOwned::new(conn, tcp))
}

/// 逐段读取直到出现 `\r\n\r\n`（响应头结束），多读的 body 前置字节返回给调用方。
fn read_headers(stream: &mut impl Read) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream
            .read(&mut chunk)
            .map_err(|e| format!("读取响应头失败: {e}"))?;
        if n == 0 {
            return Err("响应中断".to_string());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let hdr_end = buf
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4)
                .unwrap_or(buf.len());
            let body_prefix = buf[hdr_end..].to_vec();
            buf.truncate(hdr_end);
            return Ok((buf, body_prefix));
        }
        if buf.len() > 64 * 1024 {
            return Err("响应头过大".to_string());
        }
    }
}

/// 解析状态行与响应头。
fn parse_response(raw: &[u8]) -> Result<(u16, Headers), String> {
    let text = String::from_utf8_lossy(raw);
    let mut lines = text.split('\n');
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.trim().parse::<u16>().ok())
        .ok_or_else(|| format!("无法解析 HTTP 状态行: {status_line}"))?;
    let mut headers = Headers::new();
    for line in lines {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok((status, headers))
}

fn header_find<'a>(headers: &'a Headers, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// 按 framing（chunked / content-length / 读到 EOF）读取响应体。
/// `prefix` 为响应头之后已多读的 body 前置字节。
fn read_body(
    stream: &mut impl Read,
    headers: &Headers,
    mut prefix: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    if header_find(headers, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
    {
        // 从 prefix/流中按 chunk 帧结构解出（hex 大小行 + 数据 + CRLF 循环）。
        let mut need = |n: usize, prefix: &mut Vec<u8>| -> Result<Vec<u8>, String> {
            while prefix.len() < n {
                let mut chunk = [0u8; 8192];
                let got = stream
                    .read(&mut chunk)
                    .map_err(|_| "流已结束".to_string())?;
                if got == 0 {
                    return Err("流已结束".to_string());
                }
                prefix.extend_from_slice(&chunk[..got]);
            }
            Ok(prefix.drain(..n).collect())
        };
        loop {
            let mut line = Vec::new();
            loop {
                let b = need(1, &mut prefix)?;
                line.push(b[0]);
                if line.ends_with(b"\r\n") {
                    break;
                }
                if line.len() > 32 {
                    return Err("chunk 大小行过长".to_string());
                }
            }
            let size =
                usize::from_str_radix(String::from_utf8_lossy(&line[..line.len() - 2]).trim(), 16)
                    .map_err(|e| e.to_string())?;
            if size == 0 {
                return Ok(body);
            }
            if body.len() + size > MAX_BODY {
                return Err("响应体超过大小上限".to_string());
            }
            body.extend_from_slice(&need(size, &mut prefix)?);
            need(2, &mut prefix)?; // chunk 尾 CRLF
        }
    }
    if let Some(n) =
        header_find(headers, "content-length").and_then(|v| v.trim().parse::<usize>().ok())
    {
        if n > MAX_BODY {
            return Err("响应体超过大小上限".to_string());
        }
        while prefix.len() < n {
            let mut chunk = [0u8; 16384];
            let got = stream
                .read(&mut chunk)
                .map_err(|_| "流已结束".to_string())?;
            if got == 0 {
                return Err("流已结束".to_string());
            }
            prefix.extend_from_slice(&chunk[..got]);
        }
        prefix.truncate(n);
        return Ok(prefix);
    }
    // 无 framing：读到 EOF（服务器 Connection: close 场景）。
    body = prefix;
    let mut chunk = [0u8; 16384];
    loop {
        let got = stream
            .read(&mut chunk)
            .map_err(|e| format!("读取响应体失败: {e}"))?;
        if got == 0 {
            return Ok(body);
        }
        if body.len() + got > MAX_BODY {
            return Err("响应体超过大小上限".to_string());
        }
        body.extend_from_slice(&chunk[..got]);
    }
}

/// API 请求 TLS 流：rustls 优先；握手被服务器拒绝（如 mihoyo 的 WAF 对
/// rustls ClientHello 指纹回 ProtocolVersion alert，实测 curl/schannel 均正常）
/// 时回退 schannel（Windows 系统栈）。
/// 返回错误仅在 TCP/TLS 全失败时发生；写入阶段的握手告警安全（请求未发出）。
fn api_tls_connect(host: &str, port: u16) -> Result<Box<dyn ReadWrite>, String> {
    match tls_connect(host, port, READ_TIMEOUT) {
        Ok(mut t) => {
            // rustls 惰性握手：用 complete_io 显式完成（不发送应用数据）。
            // 失败（服务器对 rustls ClientHello 指纹回 ProtocolVersion alert 等）
            // → 回退 schannel（Windows 系统栈，指纹与 curl/浏览器系一致）。
            match t.conn.complete_io(&mut t.sock) {
                Ok(_) => Ok(Box::new(t)),
                Err(alert) => {
                    log::info!("rustls 握手被拒（{alert}），回退 schannel");
                    tls_connect_schannel(host, port)
                }
            }
        }
        Err(e) => Err(e),
    }
}

/// schannel 回退（Windows 系统栈）。
fn tls_connect_schannel(host: &str, port: u16) -> Result<Box<dyn ReadWrite>, String> {
    use schannel::tls_stream;
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("解析主机 {host} 失败: {e}"))?
        .next()
        .ok_or_else(|| format!("主机 {host} 无可用地址"))?;
    let tcp = std::net::TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("连接 {host}:{port} 失败: {e}"))?;
    tcp.set_nodelay(true).ok();
    tcp.set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| format!("设置读超时失败: {e}"))?;
    let cred = schannel::schannel_cred::SchannelCred::builder()
        .acquire(schannel::schannel_cred::Direction::Outbound)
        .map_err(|e| format!("schannel 凭据获取失败: {e}"))?;
    let stream = tls_stream::Builder::new()
        .domain(host)
        .connect(cred, tcp)
        .map_err(|e| format!("schannel TLS 失败: {e}"))?;
    Ok(Box::new(stream))
}

/// Read + Write 组合 trait（双 TLS 栈的统一流类型）。
trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

/// 同步请求：连接 → 发送 → 跟随重定向 → 读取响应（解码线程可直接调用）。
pub(crate) fn request_sync(
    method: &str,
    url: &str,
    headers: &Headers,
    body: Option<&str>,
) -> Result<Response, String> {
    let (status, headers, bytes) = request_bytes_sync(method, url, headers, body)?;
    Ok(Response {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// 同步请求（二进制响应体版）：HLS 分片/m3u8 等非 UTF-8 内容用。
pub(crate) fn request_bytes_sync(
    method: &str,
    url: &str,
    headers: &Headers,
    body: Option<&str>,
) -> Result<(u16, Headers, Vec<u8>), String> {
    let mut current = url.to_string();
    for _ in 0..MAX_REDIRECTS {
        let (https, host, port, path) = split_url(&current)?;
        if !https {
            return Err("明文 http 已禁用（凭据安全）".to_string());
        }
        let mut tls = api_tls_connect(&host, port)?;
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nConnection: close\r\n"
        );
        let mut has_ua = false;
        for (k, v) in headers {
            if k.eq_ignore_ascii_case("host") {
                continue;
            }
            if k.eq_ignore_ascii_case("user-agent") {
                has_ua = true;
            }
            req.push_str(&format!("{k}: {}\r\n", header_utf8(v)));
        }
        if !has_ua {
            req.push_str(&format!("User-Agent: {DEFAULT_UA}\r\n"));
        }
        if let Some(b) = body {
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        req.push_str("\r\n");
        if let Some(b) = body {
            req.push_str(b);
        }
        write!(tls, "{req}").map_err(|e| format!("发送请求失败: {e}"))?;
        tls.flush()
            .map_err(|e| format!("发送请求失败(flush): {e}"))?;
        let (raw_headers, prefix) = read_headers(&mut tls)?;
        let (status, resp_headers) = parse_response(&raw_headers)?;
        if (300..400).contains(&status) {
            let loc = header_find(&resp_headers, "location")
                .ok_or_else(|| format!("HTTP {status} 重定向缺少 Location 头"))?
                .to_string();
            // 绝对/相对 Location 均支持（相对路径沿用当前 host）。
            current = if loc.starts_with("http://") || loc.starts_with("https://") {
                loc
            } else {
                format!("https://{host}{loc}")
            };
            continue;
        }
        let resp_body = read_body(&mut tls, &resp_headers, prefix)?;
        return Ok((status, resp_headers, resp_body));
    }
    Err("重定向次数过多".to_string())
}

/// HLS 持久连接：对同一 host 复用 TCP+TLS，避免每分片/每轮 m3u8 重建连接。
///
/// 动机（2026-10-08 实测，房间 1673630）：
/// - 新建连接拉 m3u8 142~209ms，复用仅 29~31ms（省 ~120ms/次）；
/// - CDN 对快速重复新建 TCP+TLS 直接拒绝（SSL: UNEXPECTED_EOF_WHILE_READING）。
///
/// 仅供 HLS 拉流循环使用（同 host 连续请求）；低频 API 调用仍走 request_bytes_sync。
pub(crate) struct KeepAliveConn {
    stream: Option<Box<dyn ReadWrite>>,
    host: String,
    port: u16,
}

impl Default for KeepAliveConn {
    fn default() -> Self {
        Self::new()
    }
}

impl KeepAliveConn {
    pub fn new() -> Self {
        Self {
            stream: None,
            host: String::new(),
            port: 0,
        }
    }

    /// GET 并复用连接。连接被服务端关闭/超时则丢弃并重连重试一次（GET 幂等）。
    pub fn get(&mut self, url: &str, headers: &Headers, referer: &str) -> Result<Vec<u8>, String> {
        let (_, host, port, path) = split_url(url)?;
        if self.stream.is_none() || self.host != host || self.port != port {
            self.stream = Some(api_tls_connect(&host, port)?);
            self.host = host.clone();
            self.port = port;
        }
        match self.try_get(&path, &host, headers, referer) {
            Ok(body) => Ok(body),
            Err(e) => {
                // 连接可能已被服务端（空闲/复用上限）关闭：重连重试一次。
                self.stream = Some(api_tls_connect(&host, port)?);
                self.try_get(&path, &host, headers, referer)
                    .map_err(|e2| format!("{e}；重连后仍失败: {e2}"))
            }
        }
    }

    fn try_get(
        &mut self,
        path: &str,
        host: &str,
        headers: &Headers,
        referer: &str,
    ) -> Result<Vec<u8>, String> {
        let close;
        let body = {
            let stream = self
                .stream
                .as_mut()
                .ok_or_else(|| "连接未建立".to_string())?;
            let mut req = format!(
                "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nConnection: keep-alive\r\n"
            );
            let mut has_ua = false;
            for (k, v) in headers {
                if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("referer") {
                    continue;
                }
                if k.eq_ignore_ascii_case("user-agent") {
                    has_ua = true;
                }
                req.push_str(&format!("{k}: {}\r\n", header_utf8(v)));
            }
            if !has_ua {
                req.push_str(&format!("User-Agent: {DEFAULT_UA}\r\n"));
            }
            if !referer.is_empty() {
                req.push_str(&format!("Referer: {}\r\n", header_utf8(referer)));
            }
            req.push_str("\r\n");
            stream
                .write_all(req.as_bytes())
                .map_err(|e| format!("发送请求失败: {e}"))?;
            stream
                .flush()
                .map_err(|e| format!("发送请求失败(flush): {e}"))?;
            let (raw_headers, prefix) = read_headers(stream)?;
            let (status, resp_headers) = parse_response(&raw_headers)?;
            if !(200..300).contains(&status) {
                return Err(format!("HLS HTTP {status}"));
            }
            close = header_find(&resp_headers, "connection")
                .is_some_and(|v| v.eq_ignore_ascii_case("close"));
            read_body(stream, &resp_headers, prefix)?
        };
        if close {
            self.stream = None; // 服务端要求关闭，下次重连
        }
        Ok(body)
    }
}

/// GET 请求（async 包装）。
pub async fn get(url: &str, headers: Headers) -> Result<Response, String> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || request_sync("GET", &url, &headers, None))
        .await
        .map_err(|e| format!("HTTP 任务失败: {e}"))?
}

/// POST 请求（body 为原始字符串）。
pub async fn post(url: &str, headers: Headers, body: &str) -> Result<Response, String> {
    let url = url.to_string();
    let body = body.to_string();
    tokio::task::spawn_blocking(move || request_sync("POST", &url, &headers, Some(&body)))
        .await
        .map_err(|e| format!("HTTP 任务失败: {e}"))?
}
