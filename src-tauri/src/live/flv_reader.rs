use std::io::Read;
use std::time::Duration;

use super::bili::BROWSER_UA;
// ---------- http-flv 流式读取（chunked 解码 + 慢流检测）----------

/// 慢流判定读超时：CDN 节点 3 秒不推数据 → read 超时 → 换节点。
pub(super) const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// 手写 HTTP/1.1 流式读取（http-flv 用）。
///
/// 为什么不用 ureq：实测 ureq 2.12 的 `into_reader()` 对 B站 http-flv 的
/// chunked 长连接会卡死（10 秒仅读到 2 个 tag，curl 同流 60fps 正常）。
/// 这里手写 GET + chunked 解码，读超时兼作慢流检测。
pub(super) struct HttpFlvReader {
    stream: Box<dyn Read + Send>,
    /// 响应是否用 chunked 传输编码。
    /// **关键**：B站不同 CDN 服务器行为不一——JSP3 返回 chunked，Tengine 返回
    /// 裸 FLV（Connection: close 直出）。必须按响应头区分，否则裸流会被误当
    /// chunk 头解析（"FLV\x01" 被当 hex 大小行）而读不出任何数据。
    chunked: bool,
    /// 当前 chunk 剩余字节（仅 chunked 模式使用）。
    chunk_left: usize,
    done: bool,
}

impl Read for HttpFlvReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        // 裸 FLV 模式：直接透传底层流（Connection: close 直出，读到 EOF 为止）。
        if !self.chunked {
            return self.stream.read(buf);
        }
        if self.chunk_left == 0 {
            // 读 chunk 大小行（hex + CRLF）。
            let mut size_line = Vec::with_capacity(16);
            loop {
                let mut b = [0u8; 1];
                if self.stream.read(&mut b)? == 0 {
                    return Ok(0);
                }
                size_line.push(b[0]);
                if size_line.ends_with(b"\r\n") {
                    break;
                }
                if size_line.len() > 32 {
                    return Err(std::io::Error::other("chunk 大小行过长"));
                }
            }
            let hex = String::from_utf8_lossy(&size_line[..size_line.len() - 2]);
            let size = usize::from_str_radix(hex.trim(), 16).map_err(std::io::Error::other)?;
            if size == 0 {
                // 终止 chunk：吃掉结尾 CRLF。
                let mut crlf = [0u8; 2];
                let _ = self.stream.read(&mut crlf)?;
                self.done = true;
                return Ok(0);
            }
            self.chunk_left = size;
        }
        let n = buf.len().min(self.chunk_left);
        let got = self.stream.read(&mut buf[..n])?;
        self.chunk_left -= got;
        if self.chunk_left == 0 {
            // chunk 数据后的 CRLF。
            let mut crlf = [0u8; 2];
            let _ = self.stream.read(&mut crlf)?;
        }
        Ok(got)
    }
}

impl HttpFlvReader {
    /// 连接 https://... 并发起 GET，返回已就绪的流式读取器（响应头已消费）。
    ///
    /// **跟随重定向**：B站 CDN 常返回 302 跳转到实际流节点（curl -L 同行为）。
    /// 早期实现把 302 当"节点不可用"直接失败，导致永远读不到流。
    pub fn connect(url: &str, referer: &str, cookie: Option<&str>) -> Result<Self, String> {
        let mut current = url.to_string();
        // 302 跳转链一般 1~2 跳，给足 6 次冗余。
        for _ in 0..6 {
            let (https, host, port, path) =
                crate::http::split_url(&current).map_err(|e| format!("流地址解析失败: {e}"))?;
            if !https {
                return Err("明文 http 已禁用".to_string());
            }
            // rustls 优先，握手被拒时回退 schannel：B站 CDN rustls 正常；
            // 抖音 CDN 对 rustls 指纹回 ProtocolVersion（回退 schannel 后可连）。
            // 读超时兼作慢流检测（3 秒无数据换节点）。
            let mut tls = crate::http::stream_tls_connect(&host, port, STREAM_READ_TIMEOUT)?;
            let cookie_line = cookie
                .map(|c| format!("Cookie: {c}\r\n"))
                .unwrap_or_default();
            write!(
                tls,
                "GET {path} HTTP/1.1
Host: {host}
User-Agent: {BROWSER_UA}
Referer: {referer}
{cookie_line}Accept: */*
Connection: keep-alive

"
            )
            .map_err(|e| format!("发送请求失败: {e}"))?;
            // rustls Writer 有内部缓冲：必须 flush 请求才会真正发出。
            tls.flush()
                .map_err(|e| format!("发送请求失败(flush): {e}"))?;
            // 读响应头直到空行（缓冲式读取：一次读一块，避免逐字节 syscall 与
            // 单字节读在慢首包下的脆弱性）。
            let mut headers = Vec::with_capacity(1024);
            let mut buf = [0u8; 4096];
            loop {
                let n = tls
                    .read(&mut buf)
                    .map_err(|e| format!("读取响应头失败: {e}"))?;
                if n == 0 {
                    return Err("响应中断".to_string());
                }
                headers.extend_from_slice(&buf[..n]);
                if headers.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if headers.len() > 16 * 1024 {
                    return Err("响应头过大".to_string());
                }
            }
            // 只保留响应头部分（缓冲可能多读了首块 body 的前几个字节）。
            let hdr_end = headers
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4)
                .unwrap_or(headers.len());
            let leftover: Vec<u8> = headers[hdr_end..].to_vec();
            headers.truncate(hdr_end);
            // 状态行解析：HTTP/1.x <code>。
            let status = headers
                .split(|&b| b == b' ')
                .nth(1)
                .and_then(|s| std::str::from_utf8(s).ok())
                .and_then(|s| s.parse::<u16>().ok())
                .ok_or_else(|| {
                    format!(
                        "无法解析 HTTP 状态行: {}",
                        String::from_utf8_lossy(&headers[..headers.len().min(80)])
                    )
                })?;
            // 3xx 重定向：读 Location 并跟随（B站 CDN 常态）。
            if (300..400).contains(&status) {
                let loc = headers
                    .split(|&b| b == b'\n')
                    .find_map(|line| {
                        let line = line.strip_suffix(b"\r").unwrap_or(line);
                        let (k, v) = line.split_at(line.iter().position(|&b| b == b':')?);
                        if k.eq_ignore_ascii_case(b"location") {
                            Some(v[1..].trim_ascii())
                        } else {
                            None
                        }
                    })
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .ok_or_else(|| format!("HTTP {status} 重定向缺少 Location 头"))?;
                current = if loc.starts_with("http://") || loc.starts_with("https://") {
                    loc.to_string()
                } else {
                    format!("https://{host}{loc}")
                };
                continue;
            }
            if status != 200 {
                return Err(format!(
                    "直播流 HTTP {status}: {}",
                    String::from_utf8_lossy(&headers[..headers.len().min(120)])
                ));
            }
            // 依响应头判定传输编码：有 Transfer-Encoding: chunked 才走 chunked 解析。
            let chunked = headers
                .split(|&b| b == b'\n')
                .filter_map(|line| {
                    let line = line.strip_suffix(b"\r").unwrap_or(line);
                    let (k, v) = line.split_at(line.iter().position(|&b| b == b':')?);
                    Some((k, v[1..].trim_ascii()))
                })
                .any(|(k, v)| {
                    k.eq_ignore_ascii_case(b"transfer-encoding")
                        && v.eq_ignore_ascii_case(b"chunked")
                });
            log::info!(
                "直播流已连接（传输编码: {}）",
                if chunked { "chunked" } else { "raw" }
            );
            // 响应头读取时多读的 body 前置字节拼回流**最前端**（流级别前缀），
            // 让它们照常进入后续解析——直接透传会破坏 chunk 头/FLV 边界。
            let stream = std::io::Cursor::new(leftover).chain(tls);
            return Ok(HttpFlvReader {
                stream: Box::new(stream),
                chunked,
                chunk_left: 0,
                done: false,
            });
        }
        Err("重定向次数过多（>6）".to_string())
    }
}

/// 从流中精确读取 n 字节（FLV tag 头/体用）；EOF/超时返回「流已结束」。
pub(super) fn read_exact(
    reader: &mut impl Read,
    buf: &mut Vec<u8>,
    n: usize,
) -> Result<(), String> {
    buf.resize(n, 0);
    reader
        .read_exact(&mut buf[..n])
        .map_err(|_| "流已结束".to_string())
}
