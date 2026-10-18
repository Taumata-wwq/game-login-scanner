package com.taumata.scanner.plugins

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.annotation.Keep
import androidx.core.app.NotificationCompat
// MainActivity 在主包 com.taumata.game_scanner 下，本文件在 .scanner.plugins 包——
// 通知的点击跳转需要它，故显式导入（2026-10-18 编译期实测报 Unresolved reference）。
import com.taumata.game_scanner.MainActivity

/**
 * Rust ↔ Kotlin 通知桥。
 *
 * 用途：扫码过程中把「后台是否在扫码 / 正在扫哪些直播间 / 扫码成功或失败」反馈到
 * 通知栏——手机切到游戏里去扫码时，应用界面不可见，通知栏是唯一的状态出口。
 *
 * 两类通知：
 * - **常驻**（[showPersistent] / [hidePersistent]）：`setOngoing(true)`，切后台时显示
 *   当前扫码状态，回到前台立刻撤下（见 Rust 侧 visibilitychange 处理）；
 * - **即时**（[notifyOnce]）：扫码成功 / 直播间下播等一次性提醒，可被用户划掉。
 *
 * 全部走独立渠道（[CHANNEL_STATUS] / [CHANNEL_EVENT]），便于用户在系统设置里单独
 * 关闭；与屏幕捕获前台服务的渠道（`screen_capture`）分开，避免互相影响。
 *
 * @Keep：JNI 静态符号约定，R8 必须保留类名与方法名（与 ClipboardBridge 同因）。
 */
@Keep
object NotifyBridge {
    @Volatile
    private var appContext: Context? = null

    init {
        System.loadLibrary("game_login_scanner_lib")
    }

    /** 由 ScreenCapturePlugin 注册时调用：保存 Application Context 并注册 JNI 引用。 */
    @JvmStatic
    fun attach(context: Context) {
        appContext = context.applicationContext
        ensureChannels()
        nativeInit()
    }

    private external fun nativeInit()

    private const val CHANNEL_STATUS = "scanner_status"
    private const val CHANNEL_EVENT = "scanner_event"
    private const val PERSISTENT_ID = 10021

    private fun ensureChannels() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val ctx = appContext ?: return
        val manager = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        // 常驻状态：低优先级、不响铃（只是「还在跑」的提示）。
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_STATUS, "扫码状态", NotificationManager.IMPORTANCE_LOW)
                .apply {
                    description = "切到后台时显示正在扫码的直播间"
                    setShowBadge(false)
                },
        )
        // 即时事件：默认优先级，扫码成功/失败会响一下，用户可划掉。
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_EVENT, "扫码结果", NotificationManager.IMPORTANCE_DEFAULT)
                .apply { description = "扫码成功、直播间下播等提醒" },
        )
    }

    private fun contentIntent(ctx: Context): PendingIntent {
        return PendingIntent.getActivity(
            ctx,
            0,
            Intent(ctx, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_NEW_TASK
            },
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    /**
     * 显示/更新常驻通知（后台扫码状态）。
     *
     * @param title 标题（如「正在扫码」）
     * @param text  正文（如「直播间 6 · 崩坏3」或「未在扫码」）
     */
    @Keep
    @JvmStatic
    fun showPersistent(title: String, text: String) {
        val ctx = appContext ?: return
        ensureChannels()
        val n: Notification = NotificationCompat.Builder(ctx, CHANNEL_STATUS)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setContentIntent(contentIntent(ctx))
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()
        val manager = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.notify(PERSISTENT_ID, n)
    }

    /** 撤下常驻通知（回到前台 / 关掉开关时调用）。 */
    @Keep
    @JvmStatic
    fun hidePersistent() {
        val ctx = appContext ?: return
        val manager = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.cancel(PERSISTENT_ID)
    }

    /**
     * 发一条可划掉的即时通知。
     *
     * @param id   业务 id（同一 id 会覆盖上一条，避免刷屏；建议按语义固定）
     * @param title 标题
     * @param text  正文
     */
    @Keep
    @JvmStatic
    fun notifyOnce(id: Int, title: String, text: String) {
        val ctx = appContext ?: return
        ensureChannels()
        val n: Notification = NotificationCompat.Builder(ctx, CHANNEL_EVENT)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setAutoCancel(true)
            .setContentIntent(contentIntent(ctx))
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .build()
        val manager = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.notify(id, n)
    }

    /** 清除即时通知（按 id）。 */
    @Keep
    @JvmStatic
    fun cancel(id: Int) {
        val ctx = appContext ?: return
        val manager = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.cancel(id)
    }
}
