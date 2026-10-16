package com.taumata.scanner.plugins

import android.content.ClipboardManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import androidx.annotation.Keep

/**
 * Rust ↔ Kotlin 剪贴板桥（读图，低频通道）。
 *
 * 用途：扫码流程的「剪贴板优先」路径（`qr::read_clipboard_gray`）——用户把二维码
 * 图片复制到剪贴板后点扫码，无需屏幕 / 摄像头取景即可识别。
 *
 * 约定：
 * - 数据格式「宽 u32 小端 + 高 u32 小端 + RGBA」单次返回（非热路径，量级 ≤16MB@2048px）；
 * - 上下文由 ScreenCapturePlugin 注册时经 attach() 注入（该版本 Tauri 不初始化
 *   ndk_context，剪贴板访问必须持有 Context）；
 * - @Keep：JNI 静态符号约定（`Java_com_taumata_scanner_plugins_ClipboardBridge_*`），
 *   R8 必须保留类名与方法名（与 ScreenCaptureBridge 同因，2026-10-17 新增）。
 *
 * 平台限制：Android 10+ 仅前台应用可读剪贴板（后台调用返回 null）；Android 9 无限制。
 * 后台受限场景由屏幕 / 摄像头路径兜底。
 */
@Keep
object ClipboardBridge {
    @Volatile
    private var appContext: Context? = null

    init {
        System.loadLibrary("game_login_scanner_lib")
    }

    /** 由 ScreenCapturePlugin 注册时调用：保存 Application Context 并注册 JNI 引用。 */
    @JvmStatic
    fun attach(context: Context) {
        appContext = context.applicationContext
        nativeInit()
    }

    private external fun nativeInit()

    /**
     * 读取剪贴板位图（头 8 字节宽高 + RGBA）；无图 / 失败返回 null。
     *
     * 两遍解码：先只读头部探测尺寸，再按 2 的幂采样解码（防超大图内存峰值）；
     * 仍超最长边 MAX_DIM 时等比缩小（filter=false 保边缘锐利，利于二维码识别）。
     */
    @JvmStatic
    fun readImage(): ByteArray? {
        val ctx = appContext ?: return null
        val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return null
        val clip = try {
            cm.primaryClip
        } catch (_: Exception) {
            null
        } ?: return null
        if (clip.itemCount == 0) return null
        val uri = clip.getItemAt(0).uri ?: return null

        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        try {
            ctx.contentResolver.openInputStream(uri)?.use {
                BitmapFactory.decodeStream(it, null, bounds)
            }
        } catch (_: Exception) {
            return null
        }
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null

        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= MAX_DIM) {
            sample *= 2
        }
        val decodeOpts = BitmapFactory.Options().apply { inSampleSize = sample }
        val bitmap: Bitmap = try {
            ctx.contentResolver.openInputStream(uri)?.use {
                BitmapFactory.decodeStream(it, null, decodeOpts)
            } ?: return null
        } catch (_: Exception) {
            return null
        }

        val src = if (bitmap.width > MAX_DIM || bitmap.height > MAX_DIM) {
            val ratio = MAX_DIM.toFloat() / maxOf(bitmap.width, bitmap.height)
            Bitmap.createScaledBitmap(
                bitmap,
                (bitmap.width * ratio).toInt().coerceAtLeast(1),
                (bitmap.height * ratio).toInt().coerceAtLeast(1),
                false,
            )
        } else {
            bitmap
        }

        val w = src.width
        val h = src.height
        val pixels = IntArray(w * h)
        src.getPixels(pixels, 0, w, 0, 0, w, h)

        val out = ByteArray(8 + w * h * 4)
        for (i in 0 until 4) {
            out[i] = ((w shr (8 * i)) and 0xFF).toByte()
            out[4 + i] = ((h shr (8 * i)) and 0xFF).toByte()
        }
        var o = 8
        for (p in pixels) {
            out[o++] = ((p shr 16) and 0xFF).toByte()  // R
            out[o++] = ((p shr 8) and 0xFF).toByte()   // G
            out[o++] = (p and 0xFF).toByte()           // B
            out[o++] = ((p ushr 24) and 0xFF).toByte() // A
        }
        return out
    }

    private const val MAX_DIM = 2048
}
