//! JNI 导出：Kotlin `ScreenCaptureBridge`（com.taumata.scanner.plugins）的
//! native 方法实现（数据面）。
//!
//! 符号命名遵循 JNI 规范：`Java_<包路径>_<类名>_<方法名>`；Kotlin 侧方法为
//! object 的实例方法 → 第二个参数为 `this`（jobject）。包路径特意不含下划线
//! （避免 `_1` 转义，见 patch-gen.ps1 的同步目标目录）。
//!
//! 采用 jni 0.22 的 `EnvUnowned::with_env` 模式：方法体内包裹 catch_unwind，
//! 出错时按 `ThrowRuntimeExAndDefault` 策略抛出 Java 异常，异常不会跨 FFI 边界
//! 以 panic 形式传播。

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteBuffer, JObject};
use jni::sys::jint;
use jni::EnvUnowned;

use super::capture;

/// `ScreenCaptureBridge.onFrame(buffer, width, height, rowStride, pixelStride)`
///
/// 同步读取 direct buffer 的裸指针（零拷贝），拷贝入库与指纹在
/// [`capture::push_frame`] 内完成；本函数返回后 Kotlin 立即 `image.close()`，
/// 因此**不得**在返回后继续引用该缓冲。
#[no_mangle]
pub extern "system" fn Java_com_taumata_scanner_plugins_ScreenCaptureBridge_onFrame<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _this: JObject<'local>,
    buffer: JByteBuffer<'local>,
    width: jint,
    height: jint,
    row_stride: jint,
    _pixel_stride: jint,
) {
    let outcome = unowned_env.with_env(|env| -> Result<(), jni::errors::Error> {
        if width <= 0 || height <= 0 || row_stride <= 0 {
            return Ok(());
        }
        let addr = env.get_direct_buffer_address(&buffer)?;
        let cap = env.get_direct_buffer_capacity(&buffer)?;
        if addr.is_null() || cap == 0 {
            return Ok(());
        }
        // SAFETY: addr 指向 Java direct ByteBuffer 的连续内存（容量 cap）；
        // 回调为同步执行（Kotlin 在本次调用返回前不会 close image），
        // 该内存在此期间有效且在本函数内不被 JVM 移动。
        let src = unsafe { std::slice::from_raw_parts(addr, cap) };
        capture::push_frame(src, width as u32, height as u32, row_stride as usize);
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>()
}

/// `ScreenCaptureBridge.onScreenInfo(width, height)`
///
/// 授权成功、捕获建立时上报屏幕物理尺寸（供首帧到达前查询扫描区域）。
#[no_mangle]
pub extern "system" fn Java_com_taumata_scanner_plugins_ScreenCaptureBridge_onScreenInfo<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _this: JObject<'local>,
    width: jint,
    height: jint,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        if width > 0 && height > 0 {
            capture::set_screen_size(width as u32, height as u32);
        }
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>()
}

/// `ScreenCaptureBridge.onCaptureStopped()`
///
/// 捕获停止（用户从系统状态栏停止共享 / 服务销毁）：清空帧槽与去重状态。
#[no_mangle]
pub extern "system" fn Java_com_taumata_scanner_plugins_ScreenCaptureBridge_onCaptureStopped<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _this: JObject<'local>,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        capture::on_capture_stopped();
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>()
}

/// `CameraCaptureBridge.onFrame(buffer, width, height, rowStride)`
///
/// 相机 Y 平面帧（灰度）：同步读取 direct buffer 裸指针（零拷贝），拷贝与指纹在
/// [`super::camera::push_frame`] 内完成；返回后 Kotlin 立即 `image.close()`。
#[no_mangle]
pub extern "system" fn Java_com_taumata_scanner_plugins_CameraCaptureBridge_onFrame<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _this: JObject<'local>,
    buffer: JByteBuffer<'local>,
    width: jint,
    height: jint,
    row_stride: jint,
) {
    let outcome = unowned_env.with_env(|env| -> Result<(), jni::errors::Error> {
        if width <= 0 || height <= 0 || row_stride <= 0 {
            return Ok(());
        }
        let addr = env.get_direct_buffer_address(&buffer)?;
        let cap = env.get_direct_buffer_capacity(&buffer)?;
        if addr.is_null() || cap == 0 {
            return Ok(());
        }
        // SAFETY: addr 指向 Java direct ByteBuffer 的连续内存（容量 cap）；
        // 回调同步执行（Kotlin 在本次调用返回前不 close image），内存有效。
        let src = unsafe { std::slice::from_raw_parts(addr, cap) };
        super::camera::push_frame(src, width as u32, height as u32, row_stride as usize);
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>()
}

/// `CameraCaptureBridge.onCameraInfo(rotationDegrees)`
#[no_mangle]
pub extern "system" fn Java_com_taumata_scanner_plugins_CameraCaptureBridge_onCameraInfo<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _this: JObject<'local>,
    rotation_degrees: jint,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        if rotation_degrees >= 0 {
            super::camera::set_rotation(rotation_degrees as u32);
        }
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>()
}

/// `CameraCaptureBridge.onCameraStopped()`
///
/// 相机会话停止：清空帧槽与去重状态。
#[no_mangle]
pub extern "system" fn Java_com_taumata_scanner_plugins_CameraCaptureBridge_onCameraStopped<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _this: JObject<'local>,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        super::camera::on_camera_stopped();
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>()
}
