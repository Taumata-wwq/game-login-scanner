//! Android MediaCodec H.264 硬件解码（实现 crate::decode::VideoDecoder）。
//!
//! 与桌面端 mf_decode.rs 对位：
//! - 桌面：Media Foundation MFT（DXVA 硬解）→ NV12 → I420；
//! - Android：MediaCodec（ByteBuffer 模式）→ 输出 YUV → 转紧凑 I420。
//!
//! 选型依据（docs/安卓端开发文档.md 第 10.2 节）：
//! - ByteBuffer 模式（非 Surface）：需拿解码后像素送 rxing 做二维码解码；
//! - KEY_LOW_LATENCY = 1（API 30+）：显著降低解码延迟。
//!
//! 注意：MediaCodec 的异步 callback 运行在 Android codec 线程，
//! 不能直接从 Tokio async_runtime 调用；本实现使用同步 dequeue 模式。

use crate::decode::{DecodedFrame, VideoDecoder};

pub struct MediaCodecDecoder {
    // TODO(阶段4)：持有 anodecs::MediaCodec 实例与输入/输出缓冲状态。
    _private: (),
}

impl MediaCodecDecoder {
    /// 创建 H.264 硬解器。对应桌面端 MfDecoder::new()。
    pub fn new() -> Result<Self, String> {
        // TODO(阶段4)：anodecs::MediaCodec::create_decoder("video/avc") + configure + start。
        // 当前为骨架，返回明确错误以便上层在未实现时给出可读提示。
        Err("MediaCodec 解码器尚未实现（阶段 4 落地）".to_string())
    }
}

impl VideoDecoder for MediaCodecDecoder {
    fn decode(&mut self, _annexb: &[u8], _ts_ms: u32) -> Result<Vec<DecodedFrame>, String> {
        Err("MediaCodec 解码器尚未实现（阶段 4 落地）".to_string())
    }

    fn name(&self) -> &'static str {
        "MediaCodec"
    }
}
