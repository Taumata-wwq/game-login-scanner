//! Android 平台适配层。
//!
//! 设计原则（见 docs/安卓端开发文档.md）：
//! - Rust 写业务，Kotlin 写平台适配；
//! - 本模块只包含 Android 强绑定的能力：屏幕捕获、摄像头帧源、视频硬解、密钥存储。
//! - 与 Windows 对应模块（duplication / screenshot / mf_decode / clipboard / crypto::dpapi）
//!   一一对位，通过 cfg 分叉或 trait 实现切换。

pub mod camera;
pub mod capture;
pub mod clipboard;
pub mod crypto_bridge;
pub mod duplication;
pub mod http_bridge;
pub mod jni_bridge;
pub mod mediacodec_decode;
pub mod screenshot;
pub mod tunnel;
