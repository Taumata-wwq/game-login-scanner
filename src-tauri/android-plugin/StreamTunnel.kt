package com.taumata.scanner.plugins

import androidx.annotation.Keep
import java.io.InputStream
import java.io.OutputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import javax.net.ssl.SSLSocket
import javax.net.ssl.SSLSocketFactory

/**
 * 系统 TLS 流式隧道（Android）。
 *
 * 动机：抖音/米哈游的 WAF 按 TLS 指纹拒绝 rustls（真机实测：握手被拒或服务器
 * 挂起不响应）。API 请求用 [HttpBridge]（一次性缓冲），但 **FLV 流式响应**不能
 * 整体缓冲——故本对象提供**流式隧道**：
 *
 * 1. 在本机 127.0.0.1 起临时 ServerSocket；
 * 2. 返回其端口给 Rust；Rust 连该本地端口并照常写 HTTP 请求；
 * 3. 隧道接受本地连接后，用**系统 TLS**（SSLSocket）连目标 host:port，双向透传。
 */
@Keep
object StreamTunnel {

    init {
        System.loadLibrary("game_login_scanner_lib")
        nativeInit()
    }

    private external fun nativeInit()

    /**
     * 打开到 target host:port 的隧道，返回本地监听端口。
     * Rust 随后连 127.0.0.1:<port> 即得到已握手的流。
     */
    @Keep
    @JvmStatic
    fun open(host: String, port: Int): Int {
        val server = ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"))
        val localPort = server.localPort
        Thread {
            var client: Socket? = null
            var tls: SSLSocket? = null
            try {
                server.soTimeout = 10000
                val c = server.accept()
                client = c
                val s = (SSLSocketFactory.getDefault().createSocket(host, port) as SSLSocket)
                tls = s
                // 启用主机名验证（默认 SSLSocket 不校验 hostname，易受 MITM）。
                s.sslParameters = s.sslParameters.apply {
                    endpointIdentificationAlgorithm = "HTTPS"
                }
                s.startHandshake()
                val up = Thread { pump(c.getInputStream(), s.outputStream) }
                val down = Thread { pump(s.inputStream, c.outputStream) }
                up.isDaemon = true
                down.isDaemon = true
                up.start()
                down.start()
                up.join()
                down.join()
            } catch (_: Exception) {
            } finally {
                // 两端必须在这里关。原来 close 写在 try 末尾，握手失败/异常
                // 路径会跳过它，逐次遗留 fd。读方向不设超时是刻意的——FLV 长连接允许空闲。
                try { client?.close() } catch (_: Exception) {}
                try { tls?.close() } catch (_: Exception) {}
                try { server.close() } catch (_: Exception) {}
            }
        }.apply { isDaemon = true; start() }
        return localPort
    }

    private fun pump(input: InputStream, output: OutputStream) {
        val buf = ByteArray(16 * 1024)
        try {
            while (true) {
                val n = input.read(buf)
                if (n < 0) break
                output.write(buf, 0, n)
                output.flush()
            }
        } catch (_: Exception) {
        }
    }
}
