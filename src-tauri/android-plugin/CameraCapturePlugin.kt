package com.taumata.scanner.plugins

import android.Manifest
import android.app.Activity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class EmptyArgs

/**
 * 摄像头扫码插件（控制面）：Rust 经 PluginHandle::run_mobile_plugin 调用以下命令。
 *
 * - requestCamera：请求 CAMERA 运行时权限（已授权则直接 resolve）；
 * - startCapture：启动后置摄像头帧管道；
 * - stopCapture：停止；
 * - captureState：查询是否正在捕获。
 *
 * 帧数据面不经此插件（高频路径走 [CameraCaptureBridge] 的 native 方法）。
 */
@TauriPlugin(
    permissions = [
        Permission(strings = [Manifest.permission.CAMERA], alias = "camera"),
    ],
)
class CameraCapturePlugin(private val activity: Activity) : Plugin(activity) {

    @Command
    fun requestCamera(invoke: Invoke) {
        if (CameraCaptureService.hasPermission(activity)) {
            invoke.resolve()
        } else {
            requestPermissionForAlias("camera", invoke, "cameraPermissionResult")
        }
    }

    @PermissionCallback
    fun cameraPermissionResult(invoke: Invoke) {
        if (CameraCaptureService.hasPermission(activity)) {
            invoke.resolve()
        } else {
            invoke.reject("相机权限被拒绝")
        }
    }

    @Command
    fun startCapture(invoke: Invoke) {
        if (!CameraCaptureService.hasPermission(activity)) {
            invoke.reject("相机权限未授予")
            return
        }
        if (CameraCaptureService.start(activity)) {
            invoke.resolve()
        } else {
            invoke.reject("启动相机失败")
        }
    }

    @Command
    fun stopCapture(invoke: Invoke) {
        CameraCaptureService.stop()
        invoke.resolve()
    }

    @Command
    fun captureState(invoke: Invoke) {
        val obj = JSObject()
        obj.put("capturing", CameraCaptureService.isRunning)
        invoke.resolve(obj)
    }
}
