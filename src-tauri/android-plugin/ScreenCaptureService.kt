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
 * 系统约束（实现依据）
 * Android 14（API 34）起：必须在调用 getMediaProjection启动
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

    /** 当前 VirtualDisplay 的像素尺寸（用于判断旋转后是否需要重建）。 */
    private var captureW: Int = 0
    private var captureH: Int = 0

    /**
     * 帧缓冲生命周期锁：串行化「帧线程 memcpy 中」与「主线程 close ImageReader」。
     *
     * 不加锁时二者会撞车：close 会 munmap 掉帧线程正在读的 gralloc 页，
     * 直接 `Fatal signal 11 (SIGSEGV) code 1 (SEGV_MAPERR)` 崩在 memcpy 里
     * （真机实证，停止共享与旋转重建均可触发）。
     */
    private val frameLock = Any()

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

    /**
     * 横竖屏 / 折叠屏展开收起 / 分屏尺寸变化：重建 VirtualDisplay 与 ImageReader。
     *
     * 为什么必须重建：`createVirtualDisplay` 的宽高在创建时固化，Activity 又因
     * `configChanges` 声明了 orientation|screenSize 而不会重建，MediaProjection
     * 本身也不会跟随分辨率。不重建的话旋转后画面会被裁切/拉伸（真机旋转 90°
     * 后只有左上角一块内容）。这里按新的 realMetrics 重建，并把新尺寸回报给
     * Rust（`onScreenInfo`），让扫码区域与预览坐标系同步更新。
     */
    override fun onConfigurationChanged(newConfig: android.content.res.Configuration) {
        super.onConfigurationChanged(newConfig)
        if (!isRunning) return
        val metrics = DisplayMetrics()
        @Suppress("DEPRECATION")
        (getSystemService(Context.WINDOW_SERVICE) as WindowManager).defaultDisplay.getRealMetrics(metrics)
        val w = metrics.widthPixels
        val h = metrics.heightPixels
        if (w == captureW && h == captureH) return
        recreateCapture(w, h, metrics.densityDpi)
    }

    /** 按新尺寸重建 VirtualDisplay + ImageReader（复用同一个 MediaProjection）。 */
    private fun recreateCapture(w: Int, h: Int, dpi: Int) {
        val proj = projection ?: return
        // 先断开旧管道：避免旧 reader 的 surface 仍挂在 VirtualDisplay 上。
        // close 必须在 frameLock 内做，否则会 munmap 掉帧线程正在 memcpy 的缓冲
        // （旋转重建时同样会 SIGSEGV，见 frameLock 注释）。
        imageReader?.setOnImageAvailableListener(null, null)
        virtualDisplay?.release()
        virtualDisplay = null
        synchronized(frameLock) {
            imageReader?.close()
            imageReader = null
        }

        val thread = frameThread ?: HandlerThread("ScreenCaptureFrames").also {
            it.start()
            frameThread = it
        }
        val reader = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 3).apply {
            setOnImageAvailableListener({ r -> onFrameAvailable(r) }, Handler(thread.looper))
        }
        val vd = try {
            proj.createVirtualDisplay(
                "game-scanner-capture",
                w, h, dpi,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                reader.surface,
                null,
                null,
            )
        } catch (e: Exception) {
            reader.close()
            return
        }
        imageReader = reader
        virtualDisplay = vd
        captureW = w
        captureH = h
        // 新尺寸回报 Rust：扫码 rect 与预览坐标系都以此为准。
        ScreenCaptureBridge.onScreenInfo(w, h)
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

        // **顺序关键**：Android 14（API 34）起，`createVirtualDisplay`必须先
        // `registerCallback`，否则抛 IllegalStateException（"Must register a
        // callback before starting capture"）→ 授权框点「开始」后直接失败。
        // 旧代码把 registerCallback 放在 createVirtualDisplay 之后，在 API 34+
        // 设备上必然拿不到帧。
        val callback = object : MediaProjection.Callback() {
            override fun onStop() {
                // 用户从系统状态栏停止共享 / 系统回收（Android 14 起每次授权为一次性
                // 令牌，应用再次请求时必须重新弹授权框——由插件 requestCapture 负责）。
                isRunning = false
                ScreenCaptureBridge.onCaptureStopped()
                cleanup()
                stopSelf()
            }
        }
        proj.registerCallback(callback, Handler(mainLooper))
        projectionCallback = callback
        projection = proj

        val thread = HandlerThread("ScreenCaptureFrames").also { it.start() }
        val reader = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 3).apply {
            setOnImageAvailableListener({ r -> onFrameAvailable(r) }, Handler(thread.looper))
        }
        val vd = try {
            proj.createVirtualDisplay(
                "game-scanner-capture",
                w, h, dpi,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                reader.surface,
                null,
                null,
            )
        } catch (e: Exception) {
            reader.close()
            thread.quitSafely()
            cleanup()
            return false
        }

        virtualDisplay = vd
        imageReader = reader
        frameThread = thread
        captureW = w
        captureH = h

        // 供取帧侧在首帧到达前查询屏幕尺寸（scan loop 启动即需要 rects）。
        ScreenCaptureBridge.onScreenInfo(w, h)
        isRunning = true
        return true
    }

    private fun onFrameAvailable(reader: ImageReader) {
        // 与 cleanup / recreateCapture 串行化。
        //
        // 为什么必须加锁（真机实证 SIGSEGV）：帧回调跑在
        // "ScreenCaptureFrames" 线程，`ScreenCaptureBridge.onFrame` 会直接 memcpy
        // 这块 gralloc direct buffer；与此同时主线程若 `imageReader.close()`，
        // 该缓冲被 munmap → 帧线程读已释放页 → `Fatal signal 11 (SIGSEGV),
        // code 1 (SEGV_MAPERR)` in `memcpy`，fault addr 是页对齐地址。
        // 「停止共享」/ 旋转重建都会触发，属必现竞态而非偶发。
        // 取帧、拷贝、释放整段都在 frameLock 内完成：
        // - 持锁期间 reader 不会被 close，direct buffer 的页不会被 munmap；
        // - img.close() 也在锁内，避免「reader 已关但 image 仍持有其缓冲」的窗口。
        // 锁粒度是一次帧拷贝（1080×2340 RGBA 约 10 MB memcpy，个位数毫秒），
        // 主线程 close 时最多等这一帧，不会造成可感知卡顿。
        try {
            synchronized(frameLock) {
                if (!isRunning) return
                val img = try {
                    reader.acquireLatestImage()
                } catch (e: Exception) {
                    null
                } ?: return
                try {
                    val plane = img.planes.firstOrNull() ?: return
                    // 同步交给 Rust：拷贝 + 指纹在 native 侧完成，返回后即可 close。
                    ScreenCaptureBridge.onFrame(
                        plane.buffer,
                        img.width,
                        img.height,
                        plane.rowStride,
                        plane.pixelStride,
                    )
                } finally {
                    try {
                        img.close()
                    } catch (_: Throwable) {
                    }
                }
            }
        } catch (_: Throwable) {
            // 帧路径不允许异常穿透到 ImageReader 回调（否则回调被移除）。
        }
    }

    // ---------- 清理 ----------

    private fun cleanup() {
        isRunning = false
        captureW = 0
        captureH = 0
        // 先摘掉回调，再在锁内关闭 reader：确保此刻没有帧线程正持着 direct buffer
        // 做 memcpy（否则 close 会 munmap 掉正在被读的页 → SIGSEGV）。
        imageReader?.setOnImageAvailableListener(null, null)
        synchronized(frameLock) {
            imageReader?.close()
            imageReader = null
        }
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
