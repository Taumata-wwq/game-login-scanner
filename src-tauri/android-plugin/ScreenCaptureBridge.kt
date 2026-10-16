package com.taumata.scanner.plugins

import androidx.annotation.Keep
import java.nio.ByteBuffer

/**
 * Rust ↔ Kotlin 帧桥（数据面，高频通道）。
 *
 * 屏幕捕获帧经此对象直达 Rust 侧帧总线（src-tauri/src/android/jni_bridge.rs
 * 导出同名 native 符号，位于 libgame_login_scanner_lib.so）。
 *
 * 约定：
 * - 三个方法均为 object 的实例方法（JNI 符号为实例风格，Rust 侧第二参数为 jobject）；
 * - onFrame 由 ImageReader 回调线程同步调用，Rust 侧在该调用内完成拷贝与指纹，
 *   返回后 Kotlin 即 close(image)，Buffer 不可再被引用；
 * - 数据为非原地读取：buffer 的 R,G,B,A 字节序（PixelFormat.RGBA_8888）。
 *
 * 为什么 @Keep：本类经 JNI **静态符号约定**
 * （`Java_com_taumata_scanner_plugins_ScreenCaptureBridge_*`）从 Rust 侧调用，
 * R8 必须保留类名与方法名，否则符号错位 → UnsatisfiedLinkError。
 * 2026-10-17 真机实证：缺 @Keep 时该类被 R8 移除，预览帧回调链路断裂、
 * 屏幕扫码不可用（与 manifest 服务类名错误叠加暴露）。
 */
@Keep
object ScreenCaptureBridge {
    init {
        // Tauri 主库已由 generated/Rust.kt 加载，此处幂等（保险）。
        System.loadLibrary("game_login_scanner_lib")
    }

    /**
     * 推送一帧屏幕数据。
     *
     * @param buffer      ImageReader plane 的直接缓冲（direct ByteBuffer）
     * @param width       逻辑宽度（像素）
     * @param height      逻辑高度（像素）
     * @param rowStride   行跨距（字节，可能含对齐 padding）
     * @param pixelStride 像素跨距（字节，RGBA_8888 = 4）
     */
    external fun onFrame(
        buffer: ByteBuffer,
        width: Int,
        height: Int,
        rowStride: Int,
        pixelStride: Int,
    )

    /** 上报屏幕物理尺寸（授权成功、捕获建立时调用一次，供取帧前查询）。 */
    external fun onScreenInfo(width: Int, height: Int)

    /** 捕获停止（用户从系统状态栏停止共享 / 服务销毁）时通知 Rust 清理状态。 */
    external fun onCaptureStopped()
}
