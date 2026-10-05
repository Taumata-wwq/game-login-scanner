//! 单元测试集合（原 lib 内联测试迁移至此，见 build.rs / Cargo.toml 中
//! 关于 Windows 测试 manifest 的说明）。

use bh3_scanner_lib::config::{AppConfig, BiliToken, Channel, OfficialToken};
use bh3_scanner_lib::constants;
use bh3_scanner_lib::crypto;
use bh3_scanner_lib::qr::{self, QrKind};
use bh3_scanner_lib::sdk::{bili, mihoyo, official};

use serde_json::{json, Value};

// ---------- qr::extract ----------

#[test]
fn extract_in_game_ticket() {
    // B 服与官服的 PC 端游戏登录码同为 qr_code_in_game.html?ticket= 形态
    let url = "https://xxx/qr_code_in_game.html?ticket=AAA111&app_id=1&biz_key=bh3_cn";
    let qr = qr::extract(url).unwrap();
    assert_eq!(qr.kind, QrKind::InGame);
    assert_eq!(qr.ticket, "AAA111");
    assert_eq!(qr.biz_key, "bh3_cn");
}

#[test]
fn extract_in_game_defaults_biz_key() {
    // 二维码缺 biz_key 时回退默认 bh3_cn。
    let qr = qr::extract("https://x/qr_code_in_game.html?ticket=T1").unwrap();
    assert_eq!(qr.biz_key, constants::BH3_DEFAULT_BIZ_KEY);
    assert_eq!(qr.biz_key, "bh3_cn");
}

#[test]
fn extract_passport_tk() {
    let url = "https://webstatic.mihoyo.com/bh3/event/mobile.html?tk=BBB222";
    let qr = qr::extract(url).unwrap();
    assert_eq!(qr.kind, QrKind::Passport);
    assert_eq!(qr.ticket, "BBB222");
}

#[test]
fn extract_passport_tk_not_first_param() {
    let url = "https://x/mobile.html?from=game&tk=CCC333&sign=1";
    let qr = qr::extract(url).unwrap();
    assert_eq!(qr.kind, QrKind::Passport);
    assert_eq!(qr.ticket, "CCC333");
}

#[test]
fn extract_ignores_lookalike_params() {
    // token= 不是 tk=；ticket 为空视为缺失
    assert!(qr::extract("https://x/mobile.html?token=abc").is_none());
    assert!(qr::extract("https://x/qr_code_in_game.html?ticket=&app_id=1").is_none());
}

#[test]
fn extract_invalid() {
    assert!(qr::extract("https://example.com/hello").is_none());
}

#[test]
fn rgba_to_rgb_len() {
    let rgba = [255, 0, 0, 128, 0, 255, 0, 128];
    let rgb = qr::rgba_to_rgb(&rgba);
    assert_eq!(rgb, vec![255, 0, 0, 0, 255, 0]);
}

#[test]
fn decode_rgb_empty() {
    assert!(qr::decode_rgb(&[], 0, 0).is_none());
}

/// 用 rxing 编码一张二维码，再走 decode_rgb 解码，验证整条识别链路。
#[test]
fn decode_rgb_roundtrip() {
    use rxing::{BarcodeFormat, MultiFormatWriter, Writer};

    let text = "https://webstatic.mihoyo.com/bh3/event/mobile.html?tk=TESTTK123";
    let size = 200usize;
    let matrix = MultiFormatWriter
        .encode(text, &BarcodeFormat::QR_CODE, size as i32, size as i32)
        .expect("编码二维码失败");
    let mut rgb = Vec::with_capacity(size * size * 3);
    for y in 0..size {
        for x in 0..size {
            let v = if matrix.get(x as u32, y as u32) {
                0u8
            } else {
                255u8
            };
            rgb.extend_from_slice(&[v, v, v]);
        }
    }
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 小图走 2x 放大重试路径仍可解码。
#[test]
fn decode_rgb_small_image_roundtrip() {
    use rxing::{BarcodeFormat, MultiFormatWriter, Writer};

    let text = "https://x/qr_code_in_game.html?ticket=SMALL";
    let size = 120usize; // < 300，触发放大重试分支
    let matrix = MultiFormatWriter
        .encode(text, &BarcodeFormat::QR_CODE, size as i32, size as i32)
        .expect("编码二维码失败");
    let mut rgb = Vec::with_capacity(size * size * 3);
    for y in 0..size {
        for x in 0..size {
            let v = if matrix.get(x as u32, y as u32) {
                0u8
            } else {
                255u8
            };
            rgb.extend_from_slice(&[v, v, v]);
        }
    }
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 大图走 2x2 降采样路径仍可解码。
#[test]
fn decode_rgb_large_image_roundtrip() {
    use rxing::{BarcodeFormat, MultiFormatWriter, Writer};

    let text = "https://x/qr_code_in_game.html?ticket=BIG";
    // 生成 200px 二维码，嵌入 1600x1600 的白色画布中心，触发降采样分支
    let qr_size = 200usize;
    let matrix = MultiFormatWriter
        .encode(
            text,
            &BarcodeFormat::QR_CODE,
            qr_size as i32,
            qr_size as i32,
        )
        .expect("编码二维码失败");
    let size = 1600usize; // ≥ 1600 触发降采样
    let mut rgb = vec![255u8; size * size * 3];
    let offset = (size - qr_size) / 2;
    for y in 0..qr_size {
        for x in 0..qr_size {
            let v = if matrix.get(x as u32, y as u32) {
                0u8
            } else {
                255u8
            };
            let i = ((y + offset) * size + (x + offset)) * 3;
            rgb[i] = v;
            rgb[i + 1] = v;
            rgb[i + 2] = v;
        }
    }
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 反色二维码（白码黑底，游戏登录界面常见）应通过 ALSO_INVERTED 解出。
#[test]
fn decode_rgb_inverted_roundtrip() {
    use rxing::{BarcodeFormat, MultiFormatWriter, Writer};

    let text = "https://x/qr_code_in_game.html?ticket=INVERTED";
    let size = 200usize;
    let matrix = MultiFormatWriter
        .encode(text, &BarcodeFormat::QR_CODE, size as i32, size as i32)
        .expect("编码二维码失败");
    let mut rgb = Vec::with_capacity(size * size * 3);
    for y in 0..size {
        for x in 0..size {
            // 反色：模块为白，背景为黑
            let v = if matrix.get(x as u32, y as u32) {
                255u8
            } else {
                0u8
            };
            rgb.extend_from_slice(&[v, v, v]);
        }
    }
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("反色二维码解码失败");
    assert_eq!(decoded, text);
}

/// 解码附带位置信息，且位置外接框落在二维码区域内。
#[test]
fn decode_rgb_located_returns_bbox() {
    use rxing::{BarcodeFormat, MultiFormatWriter, Writer};

    let text = "https://x/qr_code_in_game.html?ticket=LOC";
    let qr_size = 200usize;
    let matrix = MultiFormatWriter
        .encode(
            text,
            &BarcodeFormat::QR_CODE,
            qr_size as i32,
            qr_size as i32,
        )
        .expect("编码二维码失败");
    let size = 800usize;
    let off_x = 300usize;
    let off_y = 200usize;
    let mut rgb = vec![255u8; size * size * 3];
    for y in 0..qr_size {
        for x in 0..qr_size {
            let v = if matrix.get(x as u32, y as u32) {
                0u8
            } else {
                255u8
            };
            let i = ((y + off_y) * size + (x + off_x)) * 3;
            rgb[i] = v;
            rgb[i + 1] = v;
            rgb[i + 2] = v;
        }
    }
    let (decoded, loc) = qr::decode_rgb_located(&rgb, size as u32, size as u32).expect("解码失败");
    assert_eq!(decoded, text);
    let b = loc.expect("应返回二维码位置");
    // 定位点只覆盖三个定位图案中心，外接框略小于二维码本体，
    // 只需保证其中心大致落在二维码区域内（ROI 用途足够）。
    let cx = b.x as i32 + b.w as i32 / 2;
    let cy = b.y as i32 + b.h as i32 / 2;
    let pad = (qr_size / 2) as i32;
    assert!(cx >= off_x as i32 - pad && cx <= (off_x + qr_size) as i32 + pad);
    assert!(cy >= off_y as i32 - pad && cy <= (off_y + qr_size) as i32 + pad);

    // 从外接框外扩后裁剪，再解码应得到同一文本（ROI 链路）
    let padded = qr::expand_box(b, 96, size as u32, size as u32);
    let (roi, rw, rh) = qr::crop_rgb(&rgb, size as u32, size as u32, padded).expect("裁剪失败");
    let roi_text = qr::decode_rgb(&roi, rw, rh).expect("ROI 解码失败");
    assert_eq!(roi_text, text);
}

/// expand_box 夹紧到图像边界。
#[test]
fn expand_box_clamps_to_bounds() {
    let b = qr::QrBox {
        x: 10,
        y: 20,
        w: 100,
        h: 100,
    };
    let out = qr::expand_box(b, 96, 200, 300);
    assert_eq!(out.x, 0);
    assert_eq!(out.y, 0);
    assert_eq!(out.x + out.w, 200);
    let c = qr::QrBox {
        x: 190,
        y: 290,
        w: 5,
        h: 5,
    };
    let out2 = qr::expand_box(c, 96, 200, 300);
    assert!(out2.x + out2.w <= 200);
    assert!(out2.y + out2.h <= 300);
}

// ---------- qr_svg ----------

#[test]
fn qr_svg_contains_svg_and_path() {
    let svg = qr::qr_svg("https://user.mihoyo.com/qr-login.html?ticket=XYZ").unwrap();
    assert!(svg.starts_with("<svg"));
    assert!(svg.contains("viewBox"));
    assert!(!svg.contains("M0,0") || svg.contains("<path")); // 有路径
}

// ---------- crypto ----------

#[test]
fn md5_known_vector() {
    assert_eq!(crypto::md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
}

#[test]
fn hmac_known_vector() {
    // RFC 4231 Test Case 1
    let digest = crypto::hmac_sha256_hex("key", "The quick brown fox jumps over the lazy dog");
    assert_eq!(
        digest,
        "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
    );
}

#[test]
fn quote_b64_symbols() {
    // Base64 中 + 与 = 需转义，/ 保留。
    assert_eq!(crypto::py_quote("a+b/c="), "a%2Bb/c%3D");
}

// ---------- config ----------

#[test]
fn config_roundtrip() {
    let mut cfg = AppConfig::default();
    cfg.upsert_bili(BiliToken {
        uid: "1".into(),
        access_key: "k".into(),
        uname: "tester".into(),
        game: "bh3".into(),
        alias: String::new(),
        remark: String::new(),
        last_login_succ: true,
        last_used_at: 0,
    });
    let s = serde_json::to_string(&cfg).unwrap();
    let back: AppConfig = serde_json::from_str(&s).unwrap();
    assert_eq!(back.active_bili("bh3").unwrap().uname, "tester");
    // 迁移用 legacy 字段保存时不再写出
    assert!(!s.contains("\"bili\":{"));
}

#[test]
fn config_sanitize() {
    let mut cfg = AppConfig {
        interval_ms: 1,
        ..Default::default()
    };
    cfg.sanitize();
    assert!(cfg.interval_ms >= 50);
    cfg.interval_ms = 9999;
    cfg.sanitize();
    assert!(cfg.interval_ms <= 5000);
}

#[test]
fn config_accepts_partial_json() {
    // serde(default)：旧版本/缺字段配置可正常加载；未知旧字段（region/channel/scan_channel）被忽略。
    let cfg: AppConfig = serde_json::from_str(
        r#"{"channel":"official","scan_channel":"auto","region":{"x":1,"y":2,"w":3,"h":4}}"#,
    )
    .unwrap();
    assert_eq!(cfg.interval_ms, 150);
    assert!(cfg.bili_accounts.is_empty());
    assert!(cfg.game_channel.is_empty());
}

#[test]
fn config_migrates_legacy_accounts() {
    let mut cfg: AppConfig = serde_json::from_str(
        r#"{"bili":{"uid":"1","access_key":"k","uname":"old","last_login_succ":true},
            "official":{"uid":"2","stoken":"s","mid":"m","last_login_succ":true}}"#,
    )
    .unwrap();
    cfg.migrate_legacy();
    assert_eq!(cfg.bili_accounts.len(), 1);
    assert_eq!(cfg.official_accounts.len(), 1);
    assert_eq!(cfg.active_bili.get("bh3").map(|s| s.as_str()), Some("1"));
    assert_eq!(cfg.active_official.get("bh3").map(|s| s.as_str()), Some("2"));
    // 旧账号归入崩坏3，渠道推断为 B 服。
    assert_eq!(
        cfg.game_channel.get("bh3").copied().flatten(),
        Some(Channel::Bilibili)
    );
}

#[test]
fn account_management() {
    let mut cfg = AppConfig::default();
    let mk = |uid: &str, uname: &str| BiliToken {
        uid: uid.into(),
        access_key: "k".into(),
        uname: uname.into(),
        game: "bh3".into(),
        alias: String::new(),
        remark: String::new(),
        last_login_succ: true,
        last_used_at: 0,
    };
    cfg.upsert_bili(mk("a", "A"));
    cfg.upsert_bili(mk("b", "B"));
    // 同 uid 重复登录应更新而非新增，并设为激活
    cfg.upsert_bili(mk("a", "A2"));
    assert_eq!(cfg.bili_accounts.len(), 2);
    assert_eq!(cfg.active_bili.get("bh3").map(|s| s.as_str()), Some("a"));
    assert!(cfg.set_current("bh3", Channel::Bilibili, "b"));
    assert_eq!(cfg.active_bili("bh3").unwrap().uname, "B");
    assert!(!cfg.set_current("bh3", Channel::Bilibili, "zzz"));
    // 删除激活账号 "b"：清空激活（不自动落到其它账号，保持用户原选择语义）。
    cfg.remove_bili("bh3", "b");
    assert!(!cfg.active_bili.contains_key("bh3"));
    // 再删除 "a" 同样无副作用。
    cfg.remove_bili("bh3", "a");
    assert!(!cfg.active_bili.contains_key("bh3"));
    assert!(cfg.active_bili("bh3").is_none());
}

#[test]
fn channel_parse() {
    assert_eq!(Channel::parse("bilibili"), Some(Channel::Bilibili));
    assert_eq!(Channel::parse("official"), Some(Channel::Official));
    assert_eq!(Channel::parse("other"), None);
}

// ---------- sdk::mihoyo ----------

#[test]
fn make_sign_matches_reference() {
    // 复现 makeSign 对 {a:1,b:"2"} 的结果：data2 = "a=1&b=2"，HMAC-SHA256。
    let mut v = json!({ "a": 1, "b": "2" });
    let sign = mihoyo::make_sign(&mut v);
    let expect = mihoyo::bh3_sign("a=1&b=2");
    assert_eq!(sign, expect);
    assert_eq!(v["sign"].as_str().unwrap(), expect);
}

#[test]
fn make_sign_ignores_existing_sign_field() {
    let mut v = json!({ "a": 1, "sign": "old" });
    let sign = mihoyo::make_sign(&mut v);
    assert_eq!(sign, mihoyo::bh3_sign("a=1"));
    assert_eq!(v["sign"].as_str().unwrap(), sign);
}

// ---------- sdk::official ----------

#[test]
fn ds_payload_known_vector() {
    let s = official::ds_payload("testsalt", "1700000000", "123456", "{\"a\":1}");
    assert!(s.starts_with("1700000000,123456,"));
    let m = "salt=testsalt&t=1700000000&r=123456&b={\"a\":1}&q=";
    assert_eq!(s, format!("1700000000,123456,{}", crypto::md5_hex(m)));
}

// ---------- crypto::DPAPI ----------

#[test]
fn dpapi_roundtrip() {
    // DPAPI 在 Windows 上应能加密后解回原文；非 Windows 上 protect 返回明文。
    let plain = "secret-access-key-123";
    let protected = crypto::protect_secret(plain);
    let restored = crypto::unprotect_secret(&protected);
    assert_eq!(restored, plain);
}

#[test]
fn dpapi_prefix_marked() {
    let protected = crypto::protect_secret("x");
    #[cfg(windows)]
    assert!(protected.starts_with(crypto::DPAPI_PREFIX));
    #[cfg(not(windows))]
    assert_eq!(protected, "x");
}

#[test]
fn unprotect_plaintext_passthrough() {
    // 旧版明文（无前缀）应原样返回，向后兼容。
    assert_eq!(crypto::unprotect_secret("legacy-plain"), "legacy-plain");
}

#[test]
fn protect_empty_is_empty() {
    assert_eq!(crypto::protect_secret(""), "");
    assert_eq!(crypto::unprotect_secret(""), "");
}

// ---------- 双渠道激活 ----------

#[test]
fn current_channel_single_selection() {
    // 各渠道内各保留一个 active 账号，但「当前账号」全局唯一（current_channel）。
    let mut cfg = AppConfig::default();
    cfg.upsert_bili(BiliToken {
        uid: "b1".into(),
        access_key: "k".into(),
        uname: "b".into(),
        game: "bh3".into(),
        alias: String::new(),
        remark: String::new(),
        last_login_succ: true,
        last_used_at: 0,
    });
    // upsert_bili 后该游戏渠道应为 B 服。
    assert_eq!(cfg.active_bili.get("bh3").map(|s| s.as_str()), Some("b1"));
    assert_eq!(
        cfg.game_channel.get("bh3").copied().flatten(),
        Some(Channel::Bilibili)
    );

    cfg.upsert_official(OfficialToken {
        uid: "o1".into(),
        stoken: "s".into(),
        mid: "m".into(),
        uname: String::new(),
        game: "bh3".into(),
        alias: String::new(),
        remark: String::new(),
        last_login_succ: true,
        last_used_at: 0,
    });
    // upsert_official 后该游戏渠道切到官服，但 B 服的 active 账号仍保留。
    assert_eq!(cfg.active_official.get("bh3").map(|s| s.as_str()), Some("o1"));
    assert_eq!(
        cfg.game_channel.get("bh3").copied().flatten(),
        Some(Channel::Official)
    );
    assert_eq!(cfg.active_bili.get("bh3").map(|s| s.as_str()), Some("b1"));

    // set_current 切回 B 服。
    assert!(cfg.set_current("bh3", Channel::Bilibili, "b1"));
    assert_eq!(
        cfg.game_channel.get("bh3").copied().flatten(),
        Some(Channel::Bilibili)
    );
    assert_eq!(cfg.current_uid("bh3"), Some("b1"));

    // 删除 B 服唯一账号 → 该游戏渠道回落官服。
    cfg.remove_bili("bh3", "b1");
    assert_eq!(
        cfg.game_channel.get("bh3").copied().flatten(),
        Some(Channel::Official)
    );
    assert_eq!(cfg.current_uid("bh3"), Some("o1"));

    // 删除官服唯一账号 → 该游戏渠道归空。
    cfg.remove_official("bh3", "o1");
    assert_eq!(cfg.game_channel.get("bh3").copied().flatten(), None);
    assert_eq!(cfg.current_uid("bh3"), None);
}

#[test]
fn legacy_config_infers_current_channel() {
    // 旧配置（无 current_channel）按已选中的渠道推断。
    let mut cfg: AppConfig = serde_json::from_str(
        r#"{"bili":{"uid":"1","access_key":"k","uname":"old","last_login_succ":true}}"#,
    )
    .unwrap();
    cfg.migrate_legacy();
    assert_eq!(
        cfg.game_channel.get("bh3").copied().flatten(),
        Some(Channel::Bilibili)
    );
}

#[test]
fn secrets_encrypted_on_save_roundtrip() {
    // encrypt_secrets / decrypt_secrets 应互逆（内存明文 → 落盘密文 → 读回明文）。
    let mut cfg = AppConfig::default();
    cfg.upsert_bili(BiliToken {
        uid: "1".into(),
        access_key: "AK-REAL".into(),
        uname: "u".into(),
        game: "bh3".into(),
        alias: String::new(),
        remark: String::new(),
        last_login_succ: true,
        last_used_at: 0,
    });
    cfg.upsert_official(OfficialToken {
        uid: "2".into(),
        stoken: "ST-REAL".into(),
        mid: "m".into(),
        uname: String::new(),
        game: "bh3".into(),
        alias: String::new(),
        remark: String::new(),
        last_login_succ: true,
        last_used_at: 0,
    });
    let mut disk = cfg.clone();
    disk.encrypt_secrets();
    // 落盘对象里敏感字段不应再是明文。
    #[cfg(windows)]
    {
        assert_ne!(disk.bili_accounts[0].access_key, "AK-REAL");
        assert_ne!(disk.official_accounts[0].stoken, "ST-REAL");
    }
    disk.decrypt_secrets();
    assert_eq!(disk.bili_accounts[0].access_key, "AK-REAL");
    assert_eq!(disk.official_accounts[0].stoken, "ST-REAL");
}

// ---------- sdk::bili ----------

#[test]
fn set_sign_contains_sign_and_timestamp() {
    let mut data: Value = serde_json::from_str(constants::BILI_USERINFO_PARAM).unwrap();
    let form = bili::set_sign(&mut data);
    assert!(form.contains("sign="));
    assert!(form.contains("timestamp="));
    assert!(form.contains("client_timestamp="));
}

#[test]
fn tv_sign_sorts_params_and_appends_sec() {
    // 输入顺序打乱，验证按 key 排序后拼盐做 MD5
    let sign = bili::tv_sign(&[
        ("ts", "0".to_string()),
        ("appkey", constants::BILI_TV_APPKEY.to_string()),
        ("local_id", "0".to_string()),
    ]);
    let raw = format!("appkey={}&local_id=0&ts=0", constants::BILI_TV_APPKEY);
    let expect = crypto::md5_hex(&format!("{raw}{}", constants::BILI_TV_APPSEC));
    assert_eq!(sign, expect);
}

// ---------- config::Game（多游戏）----------

#[test]
fn game_parse_all_variants() {
    use bh3_scanner_lib::config::Game;
    assert_eq!(Game::parse("bh3"), Some(Game::Bh3));
    assert_eq!(Game::parse("hsr"), Some(Game::Hsr));
    assert_eq!(Game::parse("genshin"), Some(Game::Genshin));
    assert_eq!(Game::parse("zzz"), Some(Game::Zzz));
    assert_eq!(Game::parse("endfield"), Some(Game::Endfield));
    assert_eq!(Game::parse("arknights"), Some(Game::Arknights));
    assert_eq!(Game::parse("unknown"), None);
}

#[test]
fn game_app_id_mihoyo_vs_hypergryph() {
    use bh3_scanner_lib::config::Game;
    assert_eq!(Game::Bh3.app_id(), 1);
    assert_eq!(Game::Hsr.app_id(), 8);
    assert_eq!(Game::Genshin.app_id(), 4);
    assert_eq!(Game::Zzz.app_id(), 12);
    // 鹰角系无米哈游 app_id。
    assert_eq!(Game::Endfield.app_id(), 0);
    assert_eq!(Game::Arknights.app_id(), 0);
}

#[test]
fn game_is_hypergryph() {
    use bh3_scanner_lib::config::Game;
    assert!(!Game::Bh3.is_hypergryph());
    assert!(Game::Endfield.is_hypergryph());
    assert!(Game::Arknights.is_hypergryph());
}

#[test]
fn game_label_non_empty() {
    use bh3_scanner_lib::config::Game;
    for g in [
        Game::Bh3,
        Game::Hsr,
        Game::Genshin,
        Game::Zzz,
        Game::Endfield,
        Game::Arknights,
    ] {
        assert!(!g.label().is_empty());
    }
}

// ---------- qr::extract 鹰角码 ----------

#[test]
fn extract_hypergryph_scan_login() {
    let url = "hypergryph://scan_login?scanId=SCAN123";
    let qr = qr::extract(url).unwrap();
    assert_eq!(qr.kind, QrKind::Hypergryph);
    assert_eq!(qr.ticket, "SCAN123");
}

// ---------- B站极验结果解析 ----------

#[test]
fn bili_parse_captcha_valid_json() {
    use bh3_scanner_lib::sdk::cred_login::bili_parse_captcha;
    // seccode 本身含 "|jordan"，JSON 传递不应受影响。
    let json = r#"{"token":"TK","challenge":"CH","validate":"VA","seccode":"VA|jordan"}"#;
    let (t, c, v, s) = bili_parse_captcha(json).unwrap();
    assert_eq!(t, "TK");
    assert_eq!(c, "CH");
    assert_eq!(v, "VA");
    assert_eq!(s, "VA|jordan");
}

#[test]
fn bili_parse_captcha_rejects_missing() {
    use bh3_scanner_lib::sdk::cred_login::bili_parse_captcha;
    // 缺字段。
    assert!(bili_parse_captcha(r#"{"token":"TK"}"#).is_err());
    // 非 JSON。
    assert!(bili_parse_captcha("TK|CH|VA|VA|jordan").is_err());
}
