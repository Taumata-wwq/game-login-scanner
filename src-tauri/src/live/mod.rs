//! 直播流捕获：B站/抖音取流地址解析 → FLV/HLS 解复用 → H.264 解码（MF 硬解，见 decode.rs）
//! → YUV 帧总线 → 预览裁剪 / 扫码取帧。
//!
//! 模块划分（每个文件只负责一段职责）：
//! - `bili`    取流地址解析与清晰度档位降级链（纯解析，可单测）
//! - `meta`    直播间元数据（标题/主播/开播时间）
//! - `frames`  帧总线数据结构、参数常量、YUV→灰度/RGBA 转换与统计
//! - `session` 会话注册表与生命周期（start / stop / 切前台后台 / 多路并存）
//! - `decode`  FLV 解码线程、追帧与重连判定
//! - `flv_reader` http-flv 流式读取（chunked 解码 + 慢流检测）
//! - `hls_decode` HLS 分片下载与解码
//! - `preview` 面向前端的预览裁剪（crop_frame）与扫码取帧（take_scan_frame）
//! - `probe`   房间探针（诊断用）
//!
//! 低延迟链路：B站 API（免登录）解析出 http-flv 拉流地址 → 专用流式连接边下边解
//! （逐 Tag 解复用，解码即存共享帧）。内置追帧机制（时间戳 vs 墙钟测积压，落后即丢弃
//! 至关键帧）；MF 硬解（低延迟模式）下端到端稳定在 0.3~0.4 秒量级。
//! 共享帧供两条消费路径：预览（crop_frame 按前端视口裁剪降采样）与扫码
//! （take_scan_frame 取新帧走二维码识别管线，seq 去重）。

mod bili;
mod decode;
mod flv_reader;
mod frames;
mod hls_decode;
mod meta;
mod preview;
mod probe;
mod session;

#[cfg(test)]
mod tests;

pub use bili::{
    codec_url, codec_urls, parse_accept_qn, parse_bili_durl, parse_bili_flv, parse_bili_flv_all,
    resolve_source_sync, resolve_stream_sync, Platform, StreamSource,
};
pub use frames::{
    avcc_config_to_annexb, avcc_to_annexb, frame_stats, reset_frame_stats, scan_output_size,
    FrameStats, ScanFrame, ScanKey, LIVE_SCAN_SCALE,
};
pub use meta::{duration_text, fetch_meta, fetch_meta_strict, RoomMeta, SessionInfo};
pub use preview::{crop_frame, take_scan_frame, FrameOut};
pub use probe::{probe_flv_url, probe_room, probe_room_platform, ProbeRoomResult};
pub use session::{
    active, active_id, diag, list_sessions, pause_all, set_active, set_scan_keepalive, start,
    stop_all, stop_room,
};
