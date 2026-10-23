use crate::prelude::*;

use crate::sdk;
use crate::AppState;

/// 获取真实设备指纹（优先用配置缓存；无则请求 getFp 并持久化）。
/// 这是米哈游登录风控的核心——假指纹会触发「请求频繁」(-1001)。
async fn ensure_device_fp(state: &tauri::State<'_, AppState>) -> Result<String, String> {
    let (device_id, cached, seed_id, seed_time) = {
        let cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            cfg.device_id.clone(),
            cfg.device_fp.clone(),
            cfg.fp_seed_id.clone(),
            cfg.fp_seed_time.clone(),
        )
    };
    if !cached.is_empty() {
        return Ok(cached);
    }
    let device_id = if device_id.is_empty() {
        constants::DSPROD_DEVICE_FP_DEFAULT.to_string()
    } else {
        device_id
    };
    // 生成 seed（UUID + 时间戳）。
    let (seed_id, seed_time) = if seed_id.is_empty() {
        (
            random_uuid_v4(),
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0))
            .to_string(),
        )
    } else {
        (seed_id, seed_time)
    };
    let fp = sdk::cred_login::fetch_device_fp(&device_id, &seed_id, &seed_time).await?;
    {
        let mut cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cfg.device_fp = fp.clone();
        cfg.fp_seed_id = seed_id;
        cfg.fp_seed_time = seed_time;
        if let Err(e) = cfg.save() {
            log::warn!("保存配置失败: {e}");
        }
    }
    Ok(fp)
}

/// 发送短信验证码（channel: official | bilibili）。
#[tauri::command]
pub(crate) async fn cred_send_code(
    state: tauri::State<'_, AppState>,
    channel: String,
    phone: String,
    aigis: Option<String>,
) -> Result<serde_json::Value, String> {
    let aigis = aigis.unwrap_or_default();
    match channel.as_str() {
        "official" => {
            let device_id = cfg_device_id(&state);
            let device_fp = ensure_device_fp(&state).await?;
            match sdk::cred_login::mihoyo_send_sms(&device_id, &device_fp, &phone, &aigis).await? {
                Some(a) => Ok(serde_json::json!({ "status": "captcha", "aigis": a })),
                None => Ok(serde_json::json!({ "status": "ok" })),
            }
        }
        // B站短信登录受风控限制（86103），不支持；登录请用密码登录。
        "bilibili" => Err("B站不支持短信登录，请使用密码登录".to_string()),
        _ => Err("未知渠道".to_string()),
    }
}

/// 验证码登录。
#[tauri::command]
pub(crate) async fn cred_login_sms(
    state: tauri::State<'_, AppState>,
    channel: String,
    game: String,
    phone: String,
    code: String,
    aigis: Option<String>,
) -> Result<serde_json::Value, String> {
    let aigis = aigis.unwrap_or_default();
    let outcome = match channel.as_str() {
        "official" => {
            let device_id = cfg_device_id(&state);
            let device_fp = ensure_device_fp(&state).await?;
            sdk::cred_login::mihoyo_login_sms(&device_id, &device_fp, &phone, &code, &aigis).await?
        }
        "bilibili" => return Err("B站不支持短信登录，请使用密码登录".to_string()),
        _ => return Err("未知渠道".to_string()),
    };
    apply_cred_outcome(&state, &game, outcome).await
}

/// 密码登录。
#[tauri::command]
pub(crate) async fn cred_login_pwd(
    state: tauri::State<'_, AppState>,
    channel: String,
    game: String,
    account: String,
    password: String,
    aigis: Option<String>,
) -> Result<serde_json::Value, String> {
    let aigis = aigis.unwrap_or_default();
    let outcome = match channel.as_str() {
        "official" => {
            let device_id = cfg_device_id(&state);
            let device_fp = ensure_device_fp(&state).await?;
            sdk::cred_login::mihoyo_login_pwd(&device_id, &device_fp, &account, &password, &aigis)
                .await?
        }
        "bilibili" => sdk::cred_login::bili_login_pwd(&account, &password, &aigis).await?,
        _ => return Err("未知渠道".to_string()),
    };
    apply_cred_outcome(&state, &game, outcome).await
}

/// 把登录结果写入配置（成功）或转成前端可渲染的 captcha 响应。
async fn apply_cred_outcome(
    state: &tauri::State<'_, AppState>,
    game: &str,
    outcome: sdk::cred_login::CredOutcome,
) -> Result<serde_json::Value, String> {
    use sdk::cred_login::CredOutcome;
    match outcome {
        CredOutcome::NeedCaptcha(data) => {
            Ok(serde_json::json!({ "status": "captcha", "aigis": data }))
        }
        CredOutcome::MiyousheOk { uid, stoken, mid } => {
            // 查询米游社昵称（失败不影响登录，回退 uid 显示）。
            let uname = sdk::official::get_uname(&uid, &stoken, &mid)
                .await
                .unwrap_or_default();
            let mut cfg = state
                .config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cfg.upsert_official(crate::config::OfficialToken {
                uid,
                stoken,
                mid,
                uname,
                game: game.to_string(),
                alias: String::new(),
                remark: String::new(),
                last_login_succ: true,
                last_used_at: 0,
            });
            if let Err(e) = cfg.save() {
                log::warn!("保存配置失败: {e}");
            }
            Ok(serde_json::json!({ "status": "ok" }))
        }
        CredOutcome::BiliOk { uid, access_key } => {
            let mut cfg = state
                .config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cfg.upsert_bili(crate::config::BiliToken {
                uid,
                access_key,
                uname: String::new(),
                game: game.to_string(),
                alias: String::new(),
                remark: String::new(),
                last_login_succ: true,
                last_used_at: 0,
            });
            if let Err(e) = cfg.save() {
                log::warn!("保存配置失败: {e}");
            }
            Ok(serde_json::json!({ "status": "ok" }))
        }
    }
}

// ---------- 鹰角账号登录（短信验证码） ----------

/// 发送鹰角登录短信验证码。
#[tauri::command]
pub(crate) async fn hypergryph_send_code(phone: String) -> Result<(), String> {
    sdk::hypergryph::send_phone_code(&phone).await
}

/// 鹰角短信验证码登录：成功则保存账号并设为激活。
#[tauri::command]
pub(crate) async fn hypergryph_login(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    phone: String,
    code: String,
) -> Result<(), String> {
    let token = sdk::hypergryph::token_by_phone_code(&phone, &code).await?;
    {
        let mut cfg = state
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let game = game_key(cfg.current_game);
        cfg.upsert_hypergryph(sdk::hypergryph::HypergryphToken {
            phone: phone.clone(),
            token,
            game,
            alias: String::new(),
            remark: String::new(),
            last_login_succ: true,
            last_used_at: sdk::hypergryph::touch_now(),
        });
        if let Err(e) = cfg.save() {
            log::warn!("保存配置失败: {e}");
        }
    }
    let _ = app.emit("accounts-updated", ());
    Ok(())
}

// ---------- 扫码登录（账号登录二维码） ----------

/// 生成指定渠道的登录二维码（主界面选中哪个渠道，就生成哪个）。
/// 返回 (渠道名, 二维码 SVG)。
#[tauri::command]
pub(crate) async fn login_qr_start(
    state: tauri::State<'_, AppState>,
    channel: String,
) -> Result<(String, String), String> {
    let ch = crate::config::Channel::parse(&channel).ok_or_else(|| "未知渠道".to_string())?;
    // 代际号：并发 start 时只有最新一次的结果会写入会话。
    let seq = {
        let mut s = state
            .login_qr_seq
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *s += 1;
        *s
    };

    let device_id = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .device_id
        .clone();

    // 仅申请所选渠道。
    let mut slots = QrLoginSlots::default();
    let svg = match ch {
        crate::config::Channel::Bilibili => {
            let bili = sdk::bili::qr_login_start().await?;
            let svg = qr::qr_svg(&bili.qr_url)?;
            slots.bili = Some(bili);
            svg
        }
        crate::config::Channel::Official => {
            let official =
                sdk::official::qr_login_start(&device_id, constants::DSPROD_DEVICE_FP_DEFAULT)
                    .await?;
            let svg = qr::qr_svg(&official.qr_url)?;
            slots.official = Some(official);
            svg
        }
    };

    // 仍是最新请求才写入；过期请求的结果静默丢弃。
    let is_latest = {
        let s = state
            .login_qr_seq
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let latest = *s == seq;
        if latest {
            *state
                .login_qrs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = slots;
        }
        latest
    };
    if !is_latest {
        return Err("已取消".to_string());
    }
    Ok((channel, svg))
}

/// 轮询内部统一结果。
#[derive(Clone)]
enum PollOutcome {
    Waiting,
    Scanned,
    Expired,
    ConfirmedBili {
        uid: String,
        access_key: String,
    },
    ConfirmedOfficial {
        uid: String,
        stoken: String,
        mid: String,
        uname: String,
    },
}

/// 轮询扫码登录状态（双渠道都轮询，先确认者生效）。
/// 返回 (状态, 渠道, uid, 昵称)；渠道为空表示与渠道无关。
#[tauri::command]
pub(crate) async fn login_qr_poll(
    state: tauri::State<'_, AppState>,
) -> Result<(String, String, String, String), String> {
    use sdk::bili::BiliQrStatus;
    use sdk::official::OfficialQrStatus;

    let device_id = state
        .config
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .device_id
        .clone();

    // 取出两个会话快照（Option 克隆；会话本体留在槽内由下方写回）。
    let (bili_snap, official_snap) = {
        let slots = state
            .login_qrs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (slots.bili.clone(), slots.official.clone())
    };
    if bili_snap.is_none() && official_snap.is_none() {
        return Err("没有进行中的扫码登录".to_string());
    }

    // 网络错误不吞会话：轮询失败时槽内会话原样保留，前端重试即可。
    let bili_poll = async {
        match &bili_snap {
            None => Ok(PollOutcome::Waiting),
            Some(s) => sdk::bili::qr_login_poll(&s.auth_code)
                .await
                .map(|st| match st {
                    BiliQrStatus::Waiting => PollOutcome::Waiting,
                    BiliQrStatus::Scanned => PollOutcome::Scanned,
                    BiliQrStatus::Expired => PollOutcome::Expired,
                    BiliQrStatus::Confirmed { uid, access_key } => {
                        PollOutcome::ConfirmedBili { uid, access_key }
                    }
                }),
        }
    };
    let official_poll = async {
        match &official_snap {
            None => Ok(PollOutcome::Waiting),
            Some(s) => sdk::official::qr_login_poll(
                &s.ticket,
                &device_id,
                constants::DSPROD_DEVICE_FP_DEFAULT,
            )
            .await
            .map(|st| match st {
                OfficialQrStatus::Waiting => PollOutcome::Waiting,
                OfficialQrStatus::Scanned => PollOutcome::Scanned,
                OfficialQrStatus::Expired => PollOutcome::Expired,
                OfficialQrStatus::Confirmed {
                    uid,
                    stoken,
                    mid,
                    uname,
                } => PollOutcome::ConfirmedOfficial {
                    uid,
                    stoken,
                    mid,
                    uname,
                },
            }),
        }
    };
    let (bili_out, official_out) = tokio::join!(bili_poll, official_poll);

    // 任一渠道确认即写入账号并返回；会话槽清空对应渠道。
    // 锁不跨 await：确认处理里每次短暂取锁。
    for out in [bili_out.clone(), official_out.clone()] {
        let out = match out {
            Ok(o) => o,
            Err(e) => return Err(e),
        };
        match out {
            PollOutcome::ConfirmedBili { uid, access_key } => {
                let uname = sdk::bili::get_user_info(&uid, &access_key)
                    .await
                    .unwrap_or_default();
                {
                    let mut cfg = state
                        .config
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let game = game_key(cfg.current_game);
                    cfg.upsert_bili(crate::config::BiliToken {
                        uid: uid.clone(),
                        access_key,
                        uname: uname.clone(),
                        game,
                        alias: String::new(),
                        remark: String::new(),
                        last_login_succ: true,
                        last_used_at: 0,
                    });
                    if let Err(e) = cfg.save() {
                        log::warn!("保存配置失败: {e}");
                    }
                }
                state
                    .login_qrs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .bili = None;
                return Ok(("confirmed".into(), "bilibili".into(), uid, uname));
            }
            PollOutcome::ConfirmedOfficial {
                uid,
                stoken,
                mid,
                mut uname,
            } => {
                // 扫码登录响应的 user_info 无 nickname，单独查询米游社获取昵称；
                // 失败则回退 uid 显示（不影响登录）。
                if uname.is_empty() {
                    uname = sdk::official::get_uname(&uid, &stoken, &mid)
                        .await
                        .unwrap_or_default();
                }
                {
                    let mut cfg = state
                        .config
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let game = game_key(cfg.current_game);
                    cfg.upsert_official(crate::config::OfficialToken {
                        uid: uid.clone(),
                        stoken,
                        mid,
                        uname: uname.clone(),
                        game,
                        alias: String::new(),
                        remark: String::new(),
                        last_login_succ: true,
                        last_used_at: 0,
                    });
                    if let Err(e) = cfg.save() {
                        log::warn!("保存配置失败: {e}");
                    }
                }
                state
                    .login_qrs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .official = None;
                let name = if uname.is_empty() {
                    format!("uid {uid}")
                } else {
                    uname
                };
                return Ok(("confirmed".into(), "official".into(), uid, name));
            }
            _ => {}
        }
    }
    // 未完成：聚合状态（任一已扫码 → scanned；任一过期 → expired；否则 waiting）。
    let mut status = "waiting";
    let mut scanned = false;
    let mut expired = false;
    for out in [bili_out, official_out] {
        match out {
            Ok(PollOutcome::Scanned) => scanned = true,
            Ok(PollOutcome::Expired) => expired = true,
            _ => {}
        }
    }
    if expired {
        // 双渠道码各自 180s 有效；过期后清空会话，前端自动重生成。
        status = "expired";
        *state
            .login_qrs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = QrLoginSlots::default();
    } else if scanned {
        status = "scanned";
    }
    Ok((
        status.to_string(),
        String::new(),
        String::new(),
        String::new(),
    ))
}

/// 取消进行中的扫码登录（关闭二维码界面时调用）。
#[tauri::command]
pub(crate) fn login_qr_cancel(state: tauri::State<'_, AppState>) {
    *state
        .login_qrs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = QrLoginSlots::default();
}

// ---------- 扫描区域 ----------
