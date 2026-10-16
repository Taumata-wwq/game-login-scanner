package com.taumata.scanner.plugins

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.ImageFormat
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.media.Image
import android.media.ImageReader
import android.os.Handler
import android.os.HandlerThread
import android.util.Size
import androidx.core.content.ContextCompat

/**
 * 相机会话管理（数据面）：Camera2 打开后置摄像头 → ImageReader(YUV_420_888)
 * → Y 平面经 [CameraCaptureBridge.onFrame] 直达 Rust 帧总线。
 *
 * 与屏幕捕获（MediaProjection + 前台服务）不同：相机在前台 Activity 内使用，
 * 无需前台服务（无后台采集需求）。本对象进程级单例，管理设备/会话/读帧线程。
 *
 * 线程模型：Camera2 回调与 ImageReader 回调均运行在专用 HandlerThread；
 * 帧同步交给 Rust（拷贝在 native 侧完成），返回后立即 close(image)。
 */
object CameraCaptureService {

    private const val TARGET_W = 1280
    private const val TARGET_H = 720

    @Volatile
    var isRunning: Boolean = false
        private set

    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var reader: ImageReader? = null
    private var thread: HandlerThread? = null
    private var handler: Handler? = null

    /** 相机是否已授权。 */
    fun hasPermission(context: Context): Boolean =
        ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
            PackageManager.PERMISSION_GRANTED

    /** 启动后置摄像头（要求已授权）。返回是否成功发起。 */
    fun start(context: Context): Boolean {
        if (isRunning) return true
        val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        val cameraId = pickBackCamera(manager) ?: return false
        // 上报 sensor 方向（预览需据此旋转；后置通常 90）。
        try {
            val ch = manager.getCameraCharacteristics(cameraId)
            val orient = ch.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 90
            CameraCaptureBridge.onCameraInfo(orient)
        } catch (_: Exception) {
            CameraCaptureBridge.onCameraInfo(90)
        }

        val t = HandlerThread("CameraFrames").also { it.start() }
        val h = Handler(t.looper)
        thread = t
        handler = h

        val r = ImageReader.newInstance(TARGET_W, TARGET_H, ImageFormat.YUV_420_888, 3).apply {
            setOnImageAvailableListener({ rd -> onFrameAvailable(rd) }, h)
        }
        reader = r

        val stateCallback = object : CameraDevice.StateCallback() {
            override fun onOpened(cam: CameraDevice) {
                device = cam
                createSession(cam, r, h)
            }

            override fun onDisconnected(cam: CameraDevice) {
                cam.close()
                stopInternal()
            }

            override fun onError(cam: CameraDevice, error: Int) {
                cam.close()
                stopInternal()
            }
        }
        return try {
            manager.openCamera(cameraId, stateCallback, h)
            isRunning = true
            true
        } catch (e: Exception) {
            stopInternal()
            false
        }
    }

    /** 停止相机（幂等）。 */
    fun stop() {
        stopInternal()
        // 通知 Rust 清理帧槽与去重状态。
        try {
            CameraCaptureBridge.onCameraStopped()
        } catch (_: Throwable) {
        }
    }

    private fun createSession(cam: CameraDevice, r: ImageReader, h: Handler) {
        val surface = r.surface
        try {
            @Suppress("DEPRECATION")
            cam.createCaptureSession(listOf(surface), object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(s: CameraCaptureSession) {
                    session = s
                    try {
                        val req = cam.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
                            addTarget(surface)
                            set(
                                CaptureRequest.CONTROL_AF_MODE,
                                CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE,
                            )
                        }
                        s.setRepeatingRequest(req.build(), null, h)
                    } catch (e: Exception) {
                        stopInternal()
                    }
                }

                override fun onConfigureFailed(s: CameraCaptureSession) {
                    stopInternal()
                }
            }, h)
        } catch (e: Exception) {
            stopInternal()
        }
    }

    private fun onFrameAvailable(r: ImageReader) {
        val image: Image = try {
            r.acquireLatestImage()
        } catch (e: Exception) {
            null
        } ?: return
        try {
            val planes = image.planes
            if (planes.size < 3) return
            // 推 YUV 三平面：Y 用于扫码（灰度），U/V 用于彩色预览。
            CameraCaptureBridge.onFrame(
                planes[0].buffer, planes[1].buffer, planes[2].buffer,
                image.width, image.height,
                planes[0].rowStride, planes[1].rowStride, planes[1].pixelStride,
            )
        } catch (_: Throwable) {
            // 帧路径不允许异常穿透（否则回调被移除）。
        } finally {
            image.close()
        }
    }

    private fun pickBackCamera(manager: CameraManager): String? {
        return try {
            for (id in manager.cameraIdList) {
                val ch = manager.getCameraCharacteristics(id)
                if (ch.get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_BACK) {
                    return id
                }
            }
            manager.cameraIdList.firstOrNull()
        } catch (e: Exception) {
            null
        }
    }

    private fun stopInternal() {
        isRunning = false
        try {
            session?.close()
        } catch (_: Exception) {
        }
        session = null
        try {
            device?.close()
        } catch (_: Exception) {
        }
        device = null
        reader?.setOnImageAvailableListener(null, null)
        reader?.close()
        reader = null
        handler = null
        thread?.quitSafely()
        thread = null
    }
}
