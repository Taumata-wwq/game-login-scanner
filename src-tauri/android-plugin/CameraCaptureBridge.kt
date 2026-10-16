package com.taumata.scanner.plugins

import java.nio.ByteBuffer

/**
 * Rust ↔ Kotlin 相机帧桥（数据面，高频通道）。
 *
 * 与 [ScreenCaptureBridge] 对位，但推送的是**相机 YUV 的 Y 平面**（亮度）——
 * 扫码只需亮度，直接跳过 YUV→RGB→灰度 两步（相机 Y 平面即灰度）。
 *
 * 约定：
 * - 方法为 object 实例方法（JNI 符号实例风格，Rust 侧第二参数 jobject）；
 * - onFrame 由 ImageReader 回调线程同步调用，Rust 侧在调用内完成拷贝，返回后
 *   Kotlin 即 close(image)，Buffer 不可再引用；
 * - buffer 为 Y 平面直接缓冲（像素跨距恒 1，行跨距可能含对齐 padding）。
 */
object CameraCaptureBridge {
    init {
        System.loadLibrary("game_login_scanner_lib")
    }

    /**
     * 推送一帧相机 Y 平面（灰度）。
     *
     * @param buffer    Y 平面直接缓冲（direct ByteBuffer）
     * @param width     图像宽度（像素）
     * @param height    图像高度（像素）
     * @param rowStride Y 平面行跨距（字节，可能含对齐 padding）
     */
    external fun onFrame(
        yBuf: ByteBuffer,
        uBuf: ByteBuffer,
        vBuf: ByteBuffer,
        width: Int,
        height: Int,
        yRowStride: Int,
        uvRowStride: Int,
        uvPixelStride: Int,
    )

    /** 上报相机 sensor 方向（度；预览旋转用）。 */
    external fun onCameraInfo(rotationDegrees: Int)

    /** 相机会话停止（用户停止 / 出错 / 服务销毁）时通知 Rust 清理状态。 */
    external fun onCameraStopped()
}
