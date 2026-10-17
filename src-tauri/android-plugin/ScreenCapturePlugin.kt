package com.taumata.scanner.plugins

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjectionManager
import android.net.Uri
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/**
 * 屏幕捕获插件（控制面）：Rust 经 PluginHandle::run_mobile_plugin 调用以下命令。
 *
 * - requestCapture：拉起系统 MediaProjection 授权对话框，授权成功后启动
 *   [ScreenCaptureService]（前台服务 + 帧管道）；
 * - stopCapture：停止捕获；
 * - captureState：查询当前是否正在捕获。
 *
 * 帧数据面不经此插件（高频路径走 ScreenCaptureBridge 的 native 方法）。
 */
@TauriPlugin
class ScreenCapturePlugin(private val activity: Activity) : Plugin(activity) {

    init {
        // 触碰 HttpBridge 触发其 object 初始化 → 向 Rust 注册系统 TLS 桥
        // （native 线程后续发起 mihoyo 域名请求需要该注册）。
        HttpBridge
        StreamTunnel
        CryptoBridge
        // 剪贴板桥需要 Context：注册时注入 Application Context 并触发其 JNI 注册
        // （扫码「剪贴板优先」路径；2026-10-17 新增）。
        ClipboardBridge.attach(activity.applicationContext)
    }

    @Command
    fun requestCapture(invoke: Invoke) {
        // 已在捕获：直接返回，避免重复弹授权框（前端每次点扫码都会调用本命令）。
        if (ScreenCaptureService.isRunning) {
            invoke.resolve()
            return
        }
        val manager =
            activity.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        startActivityForResult(invoke, manager.createScreenCaptureIntent(), "captureResult")
    }

    @ActivityCallback
    fun captureResult(invoke: Invoke, result: ActivityResult) {
        val data = result.data
        if (result.resultCode == Activity.RESULT_OK && data != null) {
            ScreenCaptureService.start(activity, result.resultCode, data)
            invoke.resolve()
        } else {
            invoke.reject("用户取消或拒绝了屏幕捕获授权")
        }
    }

    @Command
    fun stopCapture(invoke: Invoke) {
        ScreenCaptureService.stop(activity)
        invoke.resolve()
    }

    @Command
    fun captureState(invoke: Invoke) {
        val obj = JSObject()
        obj.put("capturing", ScreenCaptureService.isRunning)
        invoke.resolve(obj)
    }

    /** 把应用切到后台（屏幕扫码后自动切出，让用户去游戏里对着二维码）。 */
    @Command
    fun moveToBack(invoke: Invoke) {
        activity.moveTaskToBack(true)
        invoke.resolve()
    }

    /**
     * 用系统浏览器打开链接（设置面板的项目信息 / 更新页；仅 https）。
     *
     * 桌面端对应 `open_url` 走 `rundll32 url.dll,FileProtocolHandler`；Android 无
     * 该命令，改用 ACTION_VIEW。加 NEW_TASK 是因为这里可能从非 Activity 上下文
     * 之外的场景触发，避免 "Calling startActivity() from outside of an
     * Activity context requires the FLAG_ACTIVITY_NEW_TASK flag" 崩溃。
     */
    @Command
    fun openUrl(invoke: Invoke) {
        val url = invoke.parseArgs(OpenUrlArgs::class.java).url
        if (url.isNullOrBlank() || !url.startsWith("https://")) {
            invoke.reject("仅支持 https 链接")
            return
        }
        try {
            val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url))
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            activity.startActivity(intent)
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("打开链接失败：${e.message}")
        }
    }
}

/** `openUrl` 的参数（Tauri 移动插件以 JSON 传参）。 */
@InvokeArg
class OpenUrlArgs {
    var url: String? = null
}
