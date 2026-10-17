package com.taumata.scanner.plugins

import android.app.Activity
import android.content.Intent
import android.graphics.Bitmap
import android.os.Bundle
import android.view.ViewGroup
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout

/**
 * 渠道「网页登录」容器（应用宝 / Wegame）。
 *
 * 照参考项目 `activity/TencentLoginActivity` 与 `activity/WeGameLoginActivity`——
 * 两者除 `client_id` / `pt_3rd_aid` 外完全同构，故合并为一个 Activity，用 Intent
 * extra 区分平台。
 *
 * 流程：全屏 WebView 打开 QQ 互联移动端登录页 → 登录成功后腾讯回调到
 * `auth://www.qq.com#access_token=..&openid=..`（WebView 以 onLoadResource 上报）
 * → 把 `auth://` 换回 `https://` 后 setResult 回传（含 uin，即登录的 QQ 号）。
 *
 * 失败语义：`auth://progress/0` 表示被腾讯风控拦截 → RESULT_CANCELED；
 * 用户返回键 → RESULT_CANCELED。
 */
class ChannelWebLoginActivity : Activity() {

    private var webView: WebView? = null
    private var uin: String = ""

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val appId = intent.getStringExtra(EXTRA_APPID) ?: APPID_YYB

        val root = FrameLayout(this)
        val web = WebView(this)
        web.layoutParams =
            ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
        root.addView(web)
        setContentView(root)
        webView = web

        val settings = web.settings
        settings.javaScriptEnabled = true
        settings.cacheMode = WebSettings.LOAD_NO_CACHE

        web.webViewClient = object : WebViewClient() {
            override fun onPageStarted(view: WebView?, url: String?, favicon: Bitmap?) {
                super.onPageStarted(view, url, favicon)
                url?.let { inspect(it) }
            }

            override fun onLoadResource(view: WebView?, url: String?) {
                super.onLoadResource(view, url)
                url?.let { inspect(it) }
            }

            override fun shouldOverrideUrlLoading(view: WebView?, request: WebResourceRequest?): Boolean {
                // auth:// 为自定义 scheme，WebView 默认不会导航——这里主动拦截判定。
                val u = request?.url?.toString()
                if (u != null && inspect(u)) return true
                return super.shouldOverrideUrlLoading(view, request)
            }

            override fun onPageFinished(view: WebView?, url: String?) {
                super.onPageFinished(view, url)
                url?.let { inspect(it) }
            }
        }

        web.loadUrl(loginUrl(appId, randomString(48) + "_" + System.currentTimeMillis()))
    }

    /** 检查 URL；命中登录成功 / 明确失败则结束 Activity。返回是否已处理（拦截导航）。 */
    private fun inspect(url: String): Boolean {
        if (url.contains("uin=")) {
            val q = url.substringAfter("uin=", "").substringBefore('&').trim()
            if (Regex("^[1-9][0-9]{4,10}$").matches(q)) uin = q
        }
        if (!url.contains("auth://")) return false
        if (!url.contains("access_token")) {
            if (url.contains("progress/0")) fail("腾讯风控拦截（progress/0）")
            return false
        }
        val data = Intent()
        data.putExtra(EXTRA_RESULT_URL, url.replace("auth://", "https://"))
        data.putExtra(EXTRA_RESULT_UIN, uin)
        setResult(RESULT_OK, data)
        finish()
        return true
    }

    private fun fail(reason: String) {
        setResult(RESULT_CANCELED, Intent().putExtra(EXTRA_RESULT_ERROR, reason))
        finish()
    }

    @Deprecated("Activity 基类回调；仅用于「返回即取消」语义")
    override fun onBackPressed() {
        setResult(RESULT_CANCELED, Intent().putExtra(EXTRA_RESULT_ERROR, "用户取消登录"))
        finish()
    }

    override fun onDestroy() {
        webView?.stopLoading()
        webView?.destroy()
        webView = null
        super.onDestroy()
    }

    companion object {
        const val EXTRA_APPID = "appid"
        const val EXTRA_RESULT_URL = "result_url"
        const val EXTRA_RESULT_UIN = "result_uin"
        const val EXTRA_RESULT_ERROR = "result_error"

        /** 崩坏3 在对应平台的 client_id（照参考项目 Tencent / WeGame 的 pt_3rd_aid）。 */
        const val APPID_YYB = "1105553399"
        const val APPID_WEGAME = "1110980452"

        private const val CHARS = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"

        /** QQ 互联移动端登录页（参数照参考项目，仅 client_id/pt_3rd_aid 随平台变化）。 */
        fun loginUrl(appId: String, traceId: String): String = buildString {
            append("https://xui.ptlogin2.qq.com/cgi-bin/xlogin?appid=716027609")
            append("&pt_3rd_aid=").append(appId)
            append("&daid=381&pt_skey_valid=0&style=35")
            append("&s_url=http%3A%2F%2Fconnect.qq.com&refer_cgi=m_authorize&ucheck=1")
            append("&fall_to_wv=1&status_os=13.3.1")
            append("&redirect_uri=auth%3A%2F%2Fwww.qq.com")
            append("&client_id=").append(appId)
            append("&response_type=token&scope=get_user_info%2Cget_simple_userinfo%2Cadd_t")
            append("&sdkp=i&sdkv=3.3.8_lite&state=test&status_machine=iPhone9%2C1&switch=1")
            append("&traceid=").append(traceId)
        }

        private fun randomString(len: Int): String {
            val sb = StringBuilder(len)
            val rnd = java.util.Random()
            repeat(len) { sb.append(CHARS[rnd.nextInt(CHARS.length)]) }
            return sb.toString()
        }
    }
}
