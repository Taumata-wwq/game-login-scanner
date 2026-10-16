//! 第三方渠道元数据与统一验证入口（Android 端渠道扩展的共用层）。
//!
//! 机制（经参考项目 `bh3_login_simulation` 源码级验证）：所有渠道共用米哈游
//! `combo/granter/login/v2/login` 验证——渠道凭证由 Kotlin 侧厂商 SDK 桥产出，
//! 本模块负责 channel_id 映射与统一入口，实际请求复用 [super::mihoyo] 的实现
//! （B 服 verify 与第三方渠道同一端点、同一签名）。
//!
//! 首批渠道（依赖齐备，2026-10-17）：小米 / 应用宝 / 华为 / vivo / Wegame。
//! channel_id 映射与参考项目一致；官服(1) / B站(14) 走各自已有链路，不在此表。
//! UC(20) / OPPO(18) 因 SDK 依赖不可得暂缓；魅族(16) / 360(17) 参考项目已弃。

/// 渠道元数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelInfo {
    /// 米哈游 combo 体系的 channel_id。
    pub id: u32,
    /// 配置与前端使用的稳定 key。
    pub key: &'static str,
    /// 展示名（中文）。
    pub label: &'static str,
}

/// 支持的第三方渠道表。
pub const CHANNELS: &[ChannelInfo] = &[
    ChannelInfo {
        id: 11,
        key: "xiaomi",
        label: "小米",
    },
    ChannelInfo {
        id: 13,
        key: "yyb",
        label: "应用宝",
    },
    ChannelInfo {
        id: 15,
        key: "huawei",
        label: "华为",
    },
    ChannelInfo {
        id: 19,
        key: "vivo",
        label: "vivo",
    },
    ChannelInfo {
        id: 27,
        key: "wegame",
        label: "Wegame",
    },
];

/// 按 key 解析渠道。
pub fn parse(key: &str) -> Option<ChannelInfo> {
    CHANNELS.iter().copied().find(|c| c.key == key)
}

/// 渠道验证：渠道凭证（JSON 字符串）→ bh_info（combo 凭证）。
///
/// `data_json` 各渠道格式不同，由 Kotlin SDK 桥产出（如小米 `{"uid":..,"session":..}`）。
/// 返回的 `BhInfo` 可直接供 [crate::sdk::mihoyo::scan_confirm] 做游戏码确认。
pub async fn verify(
    key: &str,
    device: &str,
    data_json: &str,
) -> Result<super::mihoyo::BhInfo, String> {
    let info = parse(key).ok_or_else(|| format!("未知渠道: {key}"))?;
    super::mihoyo::verify_with_data(device, info.id, data_json).await
}
