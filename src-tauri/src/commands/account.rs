use crate::prelude::*;

use crate::commands::util::now_sec;
use crate::config::{Channel, Game};
use crate::sdk;
use crate::AppState;

/// 回填官服账号昵称：对 uname 为空的官服账号逐个查询米游社并保存。
/// 供前端启动时调用一次（旧账号在昵称功能上线前登录，uname 为空）。
#[tauri::command]
pub(crate) async fn refresh_official_unames(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // 收集待回填的 (uid, stoken, mid)。
    let pending: Vec<(String, String, String)> = {
        let cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cfg.official_accounts
            .iter()
            .filter(|a| a.uname.is_empty())
            .map(|a| (a.uid.clone(), a.stoken.clone(), a.mid.clone()))
            .collect()
    };
    if pending.is_empty() {
        return Ok(());
    }
    let mut changed = false;
    for (uid, stoken, mid) in pending {
        if let Ok(name) = sdk::official::get_uname(&uid, &stoken, &mid).await {
            if !name.is_empty() {
                let mut cfg = state
                    .config
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(a) = cfg.official_accounts.iter_mut().find(|a| a.uid == uid) {
                    a.uname = name;
                    changed = true;
                }
            }
        }
    }
    if changed {
        let cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(e) = cfg.save() {
            log::warn!("保存配置失败: {e}");
        }
        let _ = app.emit("accounts-updated", ());
    }
    Ok(())
}

// ---------- 账号管理 ----------

/// 选择要使用的账号（全局唯一）。
/// 第三方渠道登录（Android）：调起渠道 SDK 登录 → combo/granter 换取并校验凭证 →
/// 保存渠道账号（`cred` 落盘加密，见 `config::encrypt_secrets`）。
///
/// `game` 为所属游戏 id（与其余账号同——渠道账号按游戏独立存储与显示）。
/// 返回账号摘要（channel / uid / name），供前端刷新账号列表。
#[tauri::command]
pub(crate) async fn channel_login(
    state: tauri::State<'_, AppState>,
    channel: String,
    game: String,
) -> Result<serde_json::Value, String> {
    let info =
        crate::sdk::channel::parse(&channel).ok_or_else(|| format!("未知渠道：{channel}"))?;
    // 1) Kotlin SDK 登录（阻塞至用户完成 / 取消）
    let (cred, uid, name) = crate::channel_plugin::login(&channel).await?;
    // 2) combo/granter 换取并校验 combo 凭证（channel_id 映射见 sdk::channel）
    let device_id = {
        state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .device_id
            .clone()
    };
    crate::sdk::channel::verify(&channel, &device_id, &cred).await?;
    // 3) 保存账号（按 channel + uid 去重更新）
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = now_sec();
    if let Some(a) = cfg
        .channel_accounts
        .iter_mut()
        .find(|a| a.channel == channel && a.game == game && a.uid == uid)
    {
        a.cred = cred.clone();
        a.name = name.clone();
        a.last_login_succ = true;
        a.last_used_at = now;
    } else {
        cfg.channel_accounts.push(crate::config::ChannelAccount {
            channel: channel.clone(),
            game: game.clone(),
            uid: uid.clone(),
            name: name.clone(),
            cred: cred.clone(),
            last_login_succ: true,
            last_used_at: now,
            ..Default::default()
        });
    }
    // 登录即选中（与 B服 / 官服登录行为一致）：新账号直接成为该游戏的当前账号，
    // 用户不必再点一次选中按钮。
    cfg.set_current_channel(&game, &channel, &uid);
    cfg.save().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "channel": channel,
        "channel_label": info.label,
        "uid": uid,
        "name": name,
    }))
}

/// B 站 SDK 授权登录（Android；照参考项目 `login/Bilibili.java`）。
///
/// 与「渠道登录」分开：B 站属于既有 B 服账号体系（`bili_accounts`，含扫码/密码登录路径），
/// 这里只是多一种**授权登录**入口，产出同样形态的 `uid + access_key`，因此直接复用
/// `upsert_bili`（同时成为该游戏的当前 B 服账号 + 触发预校验）。
#[cfg(target_os = "android")]
#[tauri::command]
pub(crate) async fn bili_sdk_login(
    state: tauri::State<'_, AppState>,
    game: String,
) -> Result<serde_json::Value, String> {
    let (cred, uid, name) = crate::channel_plugin::login("bilibili").await?;
    let v: serde_json::Value =
        serde_json::from_str(&cred).map_err(|e| format!("凭证解析失败：{e}"))?;
    let uid = if uid.is_empty() {
        v.get("uid")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    } else {
        uid
    };
    let access_key = v
        .get("access_key")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    if uid.is_empty() || access_key.is_empty() {
        return Err("B 站未返回 uid / access_token".to_string());
    }
    // 先校验（换 combo 凭证）：无效令牌不入库，避免留下不可用账号。
    crate::sdk::mihoyo::verify(&uid, &access_key).await?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.upsert_bili(crate::config::BiliToken {
        uid,
        access_key,
        uname: name,
        game,
        alias: String::new(),
        remark: String::new(),
        last_login_succ: true,
        last_used_at: now_sec(),
    });
    cfg.save().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "status": "ok" }))
}

/// 非 Android：B 站 SDK 授权登录不可用（桌面走扫码 / 账号密码）。
#[cfg(not(target_os = "android"))]
#[tauri::command]
pub(crate) async fn bili_sdk_login(game: String) -> Result<serde_json::Value, String> {
    let _ = game;
    Err("B 站授权登录仅支持 Android（桌面请使用扫码 / 账号密码登录）".to_string())
}

/// 删除一个渠道账号（按 channel + game + uid）。
#[tauri::command]
pub(crate) fn delete_channel_account(
    state: tauri::State<'_, AppState>,
    channel: String,
    game: String,
    uid: String,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.channel_accounts
        .retain(|a| !(a.channel == channel && a.game == game && a.uid == uid));
    // 同步清掉「当前账号」选择，否则指向已删除账号（扫描时会找不到凭证）。
    let stale = cfg
        .active_channel
        .get(&game)
        .map(|s| s.channel == channel && s.uid == uid)
        .unwrap_or(false);
    if stale {
        cfg.active_channel.remove(&game);
    }
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
    Ok(())
}

/// 游戏登录码是通用的——任何渠道账号都能确认登录，因此这里把「当前账号」
/// 明确设为该账号：扫描时**只用它**确认，不做多渠道轮询。
#[tauri::command]
pub(crate) fn select_account(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    game: String,
    channel: String,
    uid: String,
) -> Result<(), String> {
    // 第三方渠道账号：channel 形如 "channel:xiaomi"，走独立的选择存储
    // （渠道标识是「key + uid」二元组，无法塞进 Channel 枚举）。
    if let Some(key) = channel.strip_prefix("channel:") {
        let mut cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !cfg.set_current_channel(&game, key, &uid) {
            return Err("账号不存在".to_string());
        }
        if let Err(e) = cfg.save() {
            log::warn!("保存配置失败: {e}");
        }
        return Ok(());
    }
    let channel = Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // set_current 内部会清掉该游戏的渠道账号选择（两处「当前账号」不能并存）。
    if !cfg.set_current(&game, channel, &uid) {
        return Err("账号不存在".to_string());
    }
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
    drop(cfg);
    // 切换到 B 服账号时立即后台预校验（扫描确认零等待；失败提前暴露）。
    if channel == Channel::Bilibili {
        prewarm_bili_account(&app);
    }
    Ok(())
}

/// 删除某渠道的一个账号记录。
#[tauri::command]
pub(crate) fn delete_account(
    state: tauri::State<'_, AppState>,
    game: String,
    channel: String,
    uid: String,
) -> Result<(), String> {
    let channel = Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match channel {
        Channel::Bilibili => cfg.remove_bili(&game, &uid),
        Channel::Official => {
            cfg.remove_official(&game, &uid);
            // 同步清掉该账号的 bh_info 缓存（缓存带 uid 键，本不会串号，
            // 但账号已删除就无需再占着槽位）。
            crate::sdk::mihoyo::clear_bh_info_cache(&uid);
        }
    }
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
    Ok(())
}

/// 设置账号备注名 + 备注信息（双击账号行编辑）。
#[tauri::command]
pub(crate) fn set_account_meta(
    state: tauri::State<'_, AppState>,
    game: String,
    channel: String,
    uid: String,
    alias: String,
    remark: String,
) -> Result<(), String> {
    let channel = Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let ok = match channel {
        Channel::Bilibili => cfg
            .bili_accounts
            .iter_mut()
            .find(|t| t.uid == uid && t.game == game)
            .map(|t| {
                t.alias = alias;
                t.remark = remark;
            }),
        Channel::Official => cfg
            .official_accounts
            .iter_mut()
            .find(|t| t.uid == uid && t.game == game)
            .map(|t| {
                t.alias = alias;
                t.remark = remark;
            }),
    };
    if ok.is_none() {
        return Err("账号不存在".to_string());
    }
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
    Ok(())
}

/// 按给定 uid 顺序重排某渠道账号列表（前端拖拽排序后调用）。
#[tauri::command]
pub(crate) fn reorder_accounts(
    state: tauri::State<'_, AppState>,
    game: String,
    channel: String,
    order: Vec<String>,
) -> Result<(), String> {
    let channel = Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // 仅重排当前游戏的账号：给该游戏的账号按 order 排序，其它游戏保持原相对顺序。
    let idx = |uid: &str| order.iter().position(|u| u == uid).unwrap_or(usize::MAX);
    match channel {
        Channel::Bilibili => {
            cfg.bili_accounts.sort_by_key(|t| {
                if t.game == game {
                    idx(&t.uid)
                } else {
                    usize::MAX
                }
            });
        }
        Channel::Official => {
            cfg.official_accounts.sort_by_key(|t| {
                if t.game == game {
                    idx(&t.uid)
                } else {
                    usize::MAX
                }
            });
        }
    }
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
    Ok(())
}

/// 切换游戏：保存当前游戏渠道，恢复目标游戏渠道与账号。
#[tauri::command]
pub(crate) fn switch_game(state: tauri::State<'_, AppState>, game: String) -> Result<(), String> {
    let g = Game::parse(&game).ok_or_else(|| "未知游戏".to_string())?;
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cfg.current_game == g {
        return Ok(());
    }
    // 各游戏的渠道/账号本就按 game 存储，切换只需改 current_game。
    cfg.current_game = g;
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
    Ok(())
}

/// 游戏 → 存储键（用于 game_channel 映射）。
pub(crate) fn game_key(g: Game) -> String {
    match g {
        Game::Bh3 => "bh3",
        Game::Hsr => "hsr",
        Game::Genshin => "genshin",
        Game::Zzz => "zzz",
        Game::Endfield => "endfield",
        Game::Arknights => "arknights",
    }
    .to_string()
}

/// 重排鹰角账号：仅重排当前游戏的账号，其它游戏保持原相对顺序
/// （与 reorder_accounts 的米哈游账号处理一致）。
#[tauri::command]
pub(crate) fn hypergryph_reorder(
    state: tauri::State<'_, AppState>,
    game: String,
    order: Vec<String>,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let idx = |phone: &str| order.iter().position(|p| p == phone).unwrap_or(usize::MAX);
    cfg.hypergryph_accounts.sort_by_key(|t| {
        if t.game == game {
            idx(&t.phone)
        } else {
            usize::MAX
        }
    });
    cfg.save().map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- 鹰角账号管理 ----------

/// 选择鹰角账号。
#[tauri::command]
pub(crate) fn hypergryph_select(
    state: tauri::State<'_, AppState>,
    game: String,
    phone: String,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !cfg
        .hypergryph_accounts
        .iter()
        .any(|t| t.phone == phone && t.game == game)
    {
        return Err("账号不存在".to_string());
    }
    cfg.active_hypergryph.insert(game, phone);
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
    Ok(())
}

/// 删除鹰角账号。
#[tauri::command]
pub(crate) fn hypergryph_delete(
    state: tauri::State<'_, AppState>,
    game: String,
    phone: String,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cfg.remove_hypergryph(&game, &phone);
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
    Ok(())
}

/// 设置鹰角账号备注。
#[tauri::command]
pub(crate) fn hypergryph_set_meta(
    state: tauri::State<'_, AppState>,
    game: String,
    phone: String,
    alias: String,
    remark: String,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let found = cfg
        .hypergryph_accounts
        .iter_mut()
        .find(|t| t.phone == phone && t.game == game)
        .map(|t| {
            t.alias = alias;
            t.remark = remark;
        });
    if found.is_none() {
        return Err("账号不存在".to_string());
    }
    if let Err(e) = cfg.save() {
        log::warn!("保存配置失败: {e}");
    }
    Ok(())
}

// ---------- 渠道账号登录（米哈游 / B站：验证码 / 密码） ----------

/// 读取配置中的 device_id（空则回退默认），供登录接口设备绑定。
pub(crate) fn cfg_device_id(state: &tauri::State<'_, AppState>) -> String {
    let id = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .device_id
        .clone();
    if id.is_empty() {
        constants::DSPROD_DEVICE_FP_DEFAULT.to_string()
    } else {
        id
    }
}

/// 相机 sensor 方向（度；预览旋转用）。
#[tauri::command]
pub(crate) fn camera_rotation() -> u32 {
    crate::camera_plugin::camera_rotation()
}
