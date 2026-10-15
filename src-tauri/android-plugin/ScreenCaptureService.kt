package com.taumata.scanner.plugins

import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.util.DisplayMetrics
import android.view.WindowManager
import androidx.core.app.NotificationCompat
import com.taumata.game_scanner.MainActivity

/**
 * 屏幕捕获前台服务：MediaProjection → VirtualDisplay → ImageReader(RGBA_8888)
 * → 帧经 [ScreenCaptureBridge.onFrame] 直达 Rust 帧总线。
 *
 * 系统约束（2026-10-15 实现依据）：
 * - Android 14（API 34）起：必须在调用 getMediaProjection 之前启动
 *   mediaProjection 类型前台服务，否则抛 SecurityException；
 * - 前台服务类型须在 Manifest 声明 foregroundServiceType=mediaProjection，
 *   并持有 FOREGROUND_SERVICE_MEDIA_PROJECTION 权限；
 * - 用户可在系统状态栏随时停止共享（MediaProjection.Callback.onStop）。
 *
 * 线程模型：ImageReader 回调运行在专用 HandlerThread，帧同步交给 Rust
 * （拷贝 + 指纹在 Rust 侧完成），返回后立即 close(image)。
 */
class ScreenCaptureService : Service() {

    companion object {
        private const val CHANNEL_ID = "screen-capture"
        private const val NOTIFICATION_ID = 0x5343 // "SC"

        private const val ACTION_START = "com.taumata.game_scanner.plugins.action.START"

        private const val EXTRA_RESULT_CODE = "resultCode"
        private const val EXTRA_RESULT_DATA = "resultData"

        /** 当前是否正在捕获（供插件 captureState 命令查询）。 */
        @Volatile
        var isRunning: Boolean = false
            private set

        /** 由插件在授权成功后调用：携带 MediaProjection 授权结果启动前台服务。 */
        fun start(activity: Activity, resultCode: Int, data: Intent) {
            val intent = Intent(activity, ScreenCaptureService::class.java)
                .setAction(ACTION_START)
                .putExtra(EXTRA_RESULT_CODE, resultCode)
                .putExtra(EXTRA_RESULT_DATA, data)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                activity.startForegroundService(intent)
            } else {
                activity.startService(intent)
            }
        }

        /** 停止捕获（幂等）。 */
        fun stop(context: Context) {
            context.stopService(Intent(context, ScreenCaptureService::class.java))
        }
    }

    private var projection: MediaProjection? = null
    private var virtualDisplay: VirtualDisplay? = null
    private var imageReader: ImageReader? = null
    private var frameThread: HandlerThread? = null
    private var projectionCallback: MediaProjection.Callback? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action != ACTION_START) {
            stopSelf()
            return START_NOT_STICKY
        }
        val resultCode = intent.getIntExtra(EXTRA_RESULT_CODE, Activity.RESULT_CANCELED)
        val resultData: Intent? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(EXTRA_RESULT_DATA)
        }
        if (resultData == null) {
            stopSelf()
            return START_NOT_STICKY
        }

        // Android 14 起 getMediaProjection 要求先有 mediaProjection 类型的前台服务。
        startForegroundCompat()

        if (!startCapture(resultCode, resultData)) {
            stopSelf()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        cleanup()
        super.onDestroy()
    }

    // ---------- 捕获建立 ----------

    private fun startCapture(resultCode: Int, data: Intent): Boolean {
        val manager =
            getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        val proj = try {
            manager.getMediaProjection(resultCode, data)
        } catch (e: Exception) {
            return false
        } ?: return false

        val metrics = DisplayMetrics()
        @Suppress("DEPRECATION")
        (getSystemService(Context.WINDOW_SERVICE) as WindowManager).defaultDisplay.getRealMetrics(metrics)
        val w = metrics.widthPixels
        val h = metrics.heightPixels
        val dpi = metrics.densityDpi

        val thread = HandlerThread("ScreenCaptureFrames").also { it.start() }
        val reader = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 3).apply {
            setOnImageAvailableListener({ r -> onFrameAvailable(r) }, Handler(thread.looper))
        }
        val vd = proj.createVirtualDisplay(
            "game-scanner-capture",
            w, h, dpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            reader.surface,
            null,
            null,
        )

        val callback = object : MediaProjection.Callback() {
            override fun onStop() {
                // 用户从系统状态栏停止共享 / 系统回收。
                isRunning = false
                ScreenCaptureBridge.onCaptureStopped()
                cleanup()
                stopSelf()
            }
        }
        proj.registerCallback(callback, Handler(mainLooper))

        projection = proj
        virtualDisplay = vd
        imageReader = reader
        frameThread = thread
        projectionCallback = callback

        // 供取帧侧在首帧到达前查询屏幕尺寸（scan loop 启动即需要 rects）。
        ScreenCaptureBridge.onScreenInfo(w, h)
        isRunning = true
        return true
    }

    private fun onFrameAvailable(reader: ImageReader) {
        val image = try {
            reader.acquireLatestImage()
        } catch (e: Exception) {
            null
        } ?: return
        try {
            val plane = image.planes.firstOrNull() ?: return
            // 同步交给 Rust：拷贝 + 指纹在 native 侧完成，返回后即可 close。
            ScreenCaptureBridge.onFrame(
                plane.buffer,
                image.width,
                image.height,
                plane.rowStride,
                plane.pixelStride,
            )
        } catch (_: Throwable) {
            // 帧路径不允许异常穿透到 ImageReader 回调（否则回调被移除）。
        } finally {
            image.close()
        }
    }

    // ---------- 清理 ----------

    private fun cleanup() {
        isRunning = false
        imageReader?.setOnImageAvailableListener(null, null)
        imageReader?.close()
        imageReader = null
        virtualDisplay?.release()
        virtualDisplay = null
        projectionCallback?.let { cb ->
            try {
                projection?.unregisterCallback(cb)
            } catch (_: Exception) {
            }
        }
        projectionCallback = null
        projection?.stop()
        projection = null
        frameThread?.quitSafely()
        frameThread = null
    }

    // ---------- 前台通知 ----------

    private fun startForegroundCompat() {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "屏幕捕获",
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = "屏幕扫码运行期间保持捕获"
                setShowBadge(false)
            }
            manager.createNotificationChannel(channel)
        }

        val contentIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_menu_view)
            .setContentTitle("扫码器正在运行")
            .setContentText("正在共享屏幕以识别登录二维码")
            .setOngoing(true)
            .setContentIntent(contentIntent)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }
}
