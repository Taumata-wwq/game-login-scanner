package com.taumata.scanner.plugins

import android.app.Activity
import android.content.Context
import android.media.projection.MediaProjectionManager
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
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
}
