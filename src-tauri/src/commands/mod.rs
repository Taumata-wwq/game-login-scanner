//! Tauri 命令实现（按领域拆分）。
//!
//! 每个文件只放一类命令：配置、窗口、账号、登录、直播、预览、扫码、更新、日志、
//! 通知、诊断与工具。命令本身保持原签名不变，由 [crate] 顶层统一重导出，
//! 前端 invoke 名称与 `generate_handler!` 列表均不受拆分影响。

pub mod account;
pub mod applog;
pub mod camera;
pub mod config;
pub mod diag;
pub mod live;
pub mod log;
pub mod login;
pub mod notify;
pub mod preview;
pub mod scan;
pub mod update;
pub mod util;
pub mod window;
