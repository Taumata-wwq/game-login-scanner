use crate::prelude::*;

use crate::AppState;

/// 生成 UUID-v4 形态的随机串（seed_id 用）。
///
/// 不引入 uuid crate：seed 只需「每次随机 + UUID 形态」（服务端不校验版本位，
/// 原实现即为 `Uuid::new_v4()`），用已有的 rand 直接生成 16 字节并按 v4 规范
/// 置版本/变体位。
pub(crate) fn random_uuid_v4() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}

pub(crate) fn now_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 扫码成功频率限制：2 分钟内最多 10 次。超限返回错误信息。
pub(crate) fn check_scan_throttle(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let now = now_sec();
    let mut times = state
        .scan_success_times
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    times.retain(|t| now - *t < 120);
    if times.len() >= 10 {
        return Err("2 分钟内扫码成功次数已达上限（10 次），请稍后再试".to_string());
    }
    times.push(now);
    Ok(())
}

// ---------- 配置 ----------

/// 记住「最后观看的直播间」（`{platform}:{room}`；空串 = 回到桌面）。
/// 切换主画面 / 退出直播模式时调用；下次启动据此恢复。
#[tauri::command]
pub(crate) fn set_last_live(id: String, state: tauri::State<'_, AppState>) {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cfg.last_live_id != id {
        cfg.last_live_id = id;
        if let Err(e) = cfg.save() {
            log::warn!("保存配置失败: {e}");
        }
    }
}
