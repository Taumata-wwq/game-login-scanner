//! 第三方渠道元数据与统一验证入口（Android 端渠道扩展的共用层）。
//!
//! 机制（经参考项目 `bh3_login_simulation` 源码级验证）：所有渠道共用米哈游
//! `combo/granter/login/v2/login` 验证——渠道凭证由 Kotlin 侧厂商 SDK 桥产出，
//! 本模块负责 channel_id 映射与统一入口，实际请求复用 [super::mihoyo] 的实现
//! （B 服 verify 与第三方渠道同一端点、同一签名）。
//!
//! 已实装渠道（channel_id 与参考项目逐字一致）：小米(11) / 应用宝(13) / 华为(15) /
//! OPPO(18) / vivo(19) / UC 九游(20) / Wegame(27)。官服(1) / B站(14) 走各自已有
//! 链路，不在此表；接入新渠道只需在本表补 ChannelInfo（Rust 侧其余零改动）。
//!
//! 2026-10-22 补齐 UC(20) / OPPO(18)：SDK 来自参考快照 `app/libs`
//! （UC `net-sdk-9.2.4.4.aar` 6.2 MB 含 `UCGameSdk`；OPPO `gamesdk-20220915.aar`
//! 10.5 MB + `gamesdk-common-20220915.aar` 含 `GameCenterSDK`），放在
//! `vendor/channel_sdk/`（不入库）；Kotlin 侧实现见 `ChannelLoginPlugin.kt`。
//! 魅族(16) / 360(17) 参考项目源码全文为注释、无二进制，确属已弃，不做。

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
        id: 18,
        key: "oppo",
        label: "OPPO",
    },
    ChannelInfo {
        id: 20,
        key: "uc",
        label: "UC（九游）",
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
