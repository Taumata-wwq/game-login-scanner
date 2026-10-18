package com.taumata.scanner.plugins

import android.util.Base64
import android.util.Log
import androidx.annotation.Keep
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL

/**
 * 系统 TLS HTTP 桥（Android）。
 *
 * 动机：米哈游 WAF 按 TLS ClientHello 指纹拒绝 rustls（真机实测
 * `passport-api.mihoyo.com` 回 `ProtocolVersion` alert），而 Windows 靠
 * schannel 系统栈绕过——Android 无 schannel。本对象用 [HttpURLConnection]
 * （底层走系统 TLS/Conscrypt，指纹与官方 App 一致）代为发起这些请求。
 *
 * 接口：全部以字符串进出，返回 JSON（status/headers/bodyBase64 或 error）。
 * Rust 侧 [crate::android::http_bridge] 经 JNI 调用（同步）。
 */
@Keep
object HttpBridge {

    private const val TAG = "GameScanner"

    init {
        System.loadLibrary("game_login_scanner_lib")
        // 把 HttpBridge 的全局引用与 JavaVM 注册给 Rust（native 线程后续复用）。
        nativeInit()
    }

    /** 注册 global ref 与 JavaVM 到 Rust（object 初始化时自动调用一次）。 */
    private external fun nativeInit()

    /**
     * 发起一次 HTTP(S) 请求。
     *
     * @param method      HTTP 方法（GET/POST）
     * @param url         完整 URL（https）
     * @param headersJson 请求头 JSON 对象（字符串→字符串）
     * @param body        请求体（UTF-8；null 表示无体）
     * @return JSON：成功 {"status":200,"headers":{...},"bodyBase64":"..."}；
     *         失败 {"error":"..."}
     */
    @Keep
    @JvmStatic
    fun request(method: String, url: String, headersJson: String, body: String?): String {
        return try {
            val conn = URL(url).openConnection() as HttpURLConnection
            conn.requestMethod = method
            conn.instanceFollowRedirects = false
            conn.connectTimeout = 10000
            conn.readTimeout = 20000
            val hdr = JSONObject(headersJson)
            hdr.keys().forEach { k -> conn.setRequestProperty(k, hdr.getString(k)) }
            if (body != null) {
                conn.doOutput = true
                conn.outputStream.use { it.write(body.toByteArray(Charsets.UTF_8)) }
            }
            val status = conn.responseCode
            val respHeaders = JSONObject()
            conn.headerFields.forEach { (k, v) ->
                if (k != null && v != null && v.isNotEmpty()) {
                    respHeaders.put(k, v.joinToString(", "))
                }
            }
            val stream = if (status >= 400) conn.errorStream else conn.inputStream
            val bytes = stream?.readBytes() ?: ByteArray(0)
            val out = JSONObject()
            out.put("status", status)
            out.put("headers", respHeaders)
            out.put("bodyBase64", Base64.encodeToString(bytes, Base64.NO_WRAP))
            out.toString()
        } catch (e: Exception) {
            // 写 logcat：Rust 侧只把 error 串成文案回传前端，而**异常类名**（如
            // SSLHandshakeException / UnknownHostException / SocketTimeoutException）
            // 是区分「TLS 指纹被拒 / DNS / 超时 / 代理」的唯一线索——必须留在设备日志里。
            Log.w(TAG, "HttpBridge 请求失败: $method $url -> ${e.javaClass.name}: ${e.message}", e)
            val out = JSONObject()
            out.put("error", e.toString())
            out.toString()
        }
    }
}
