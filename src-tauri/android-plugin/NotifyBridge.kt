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
// 通知的点击跳转需要它，故显式导入（编译期实测报 Unresolved reference）。
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

    /**
     * MainActivity 注册的「停止扫码」回调（Activity 存活期有效）。
     *
     * 用回调而不是直接持 MainActivity 引用：Activity 重建（旋转/回收）后旧引用会泄漏。
     * Activity 在 onCreate 里注册、onDestroy 里清除。
     */
    @Volatile
    private var stopScanCallback: (() -> Unit)? = null

    /** MainActivity onCreate 调用。 */
    @JvmStatic
    fun setStopScanCallback(cb: () -> Unit) {
        stopScanCallback = cb
    }

    /** MainActivity onDestroy 调用，避免泄漏。 */
    @JvmStatic
    fun clearStopScanCallback() {
        stopScanCallback = null
    }

    /** 收到「停止扫码」广播时转发给 Activity（不在则忽略）。 */
    @JvmStatic
    fun onStopScanAction() {
        stopScanCallback?.invoke()
    }

    private external fun nativeInit()

    private const val CHANNEL_STATUS = "scanner_status"
    private const val CHANNEL_EVENT = "scanner_event"
    private const val PERSISTENT_ID = 10021

    // Kotlin 的 `object` 本身就是单例：常量直接写在里面即可，**不能再套 companion
    // object**（真机构建报 "Modifier 'companion' is not applicable inside 'standalone
    // object'"，并连带 EXTRA_OPEN_TARGET / ACTION_STOP_SCAN 全部 Unresolved）。
    /** 通知 → MainActivity 的跳转目标 extra（`live:<platform>:<room>`）。 */
    const val EXTRA_OPEN_TARGET = "gs_open_target"

    /** 「停止扫码」动作广播（由 [NotifyActionReceiver] 接收后转发给前端）。 */
    const val ACTION_STOP_SCAN = "com.taumata.game_scanner.action.STOP_SCAN"

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

    /**
     * 通知点击后的跳转意图。
     *
     * 除「回到应用」外还带一个 `gs_open_target` extra（形如 `live:bilibili:6`）——
     * MainActivity 在 `onNewIntent` 里读到它后会调 `window.__gsOpenTarget(...)`，
     * 把界面**直接定位到该直播间**。用户从通知进来时最想要的就是这个，否则还得
     * 自己在侧栏里找（尤其后台在扫多个房间时）。
     *
     * `flag = UPDATE_CURRENT` 保证 extra 随每次 notify 更新；若用默认的
     * FLAG_UPDATE_CURRENT 之外的标志，旧 extra 会被缓存下来（点第二条通知仍跳第一个房间）。
     */
    private fun contentIntent(ctx: Context, target: String?): PendingIntent {
        val intent = Intent(ctx, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_NEW_TASK
            if (!target.isNullOrEmpty()) putExtra(EXTRA_OPEN_TARGET, target)
        }
        // requestCode 用 target 的哈希：不同直播间各自一个 PendingIntent，
        // 否则会被系统按"同一 intent"合并，extra 串味。
        val reqCode = target?.hashCode() ?: 0
        return PendingIntent.getActivity(
            ctx,
            reqCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    /**
     * 显示/更新常驻通知（后台扫码状态）。
     *
     * @param title  标题（如「正在扫码」）
     * @param text   正文（如「屏幕 · 崩坏3」或「未在扫码」）
     * @param target 点击后要定位的目标（`live:<platform>:<room>`；空则只回主界面）
     */
    @Keep
    @JvmStatic
    fun showPersistent(title: String, text: String, target: String?) {
        val ctx = appContext ?: return
        ensureChannels()
        val builder = NotificationCompat.Builder(ctx, CHANNEL_STATUS)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setContentIntent(contentIntent(ctx, target))
            .setPriority(NotificationCompat.PRIORITY_LOW)
        // 「停止扫码」动作：用户在游戏里扫完想停时，不必再切回应用找按钮
        // （切回应用还会因「回到应用后自动停止屏幕扫码」的开关状态而不确定停没停）。
        // 用 BroadcastReceiver 接收——它不依赖 Activity 存活，后台也能立刻生效。
        builder.addAction(
            android.R.drawable.ic_media_pause,
            "停止扫码",
            stopScanBroadcast(ctx),
        )
        val manager = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.notify(PERSISTENT_ID, builder.build())
    }

    /** 「停止扫码」广播：MainActivity 收到后转发给前端（见 ACTION_STOP_SCAN 常量）。 */
    private fun stopScanBroadcast(ctx: Context): PendingIntent {
        val intent = Intent(ctx, NotifyActionReceiver::class.java).apply {
            action = ACTION_STOP_SCAN
        }
        return PendingIntent.getBroadcast(
            ctx,
            1,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
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
     * @param id     业务 id（同一 id 覆盖上一条，避免刷屏）
     * @param title  标题
     * @param text   正文
     * @param target 点击后定位的目标（`live:<platform>:<room>`；空则只回主界面）
     */
    @Keep
    @JvmStatic
    fun notifyOnce(id: Int, title: String, text: String, target: String?) {
        val ctx = appContext ?: return
        ensureChannels()
        val n: Notification = NotificationCompat.Builder(ctx, CHANNEL_EVENT)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setAutoCancel(true)
            .setContentIntent(contentIntent(ctx, target))
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
