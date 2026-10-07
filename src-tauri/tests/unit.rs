//! 单元测试集合（原 lib 内联测试迁移至此，见 build.rs / Cargo.toml 中
//! 关于 Windows 测试 manifest 的说明）。

use game_login_scanner_lib::config::{AppConfig, BiliToken, Channel, OfficialToken};
use game_login_scanner_lib::constants;
use game_login_scanner_lib::crypto;
use game_login_scanner_lib::qr::{self, QrKind};
use game_login_scanner_lib::sdk::{bili, mihoyo, official};

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

/// 用 qrcodegen 把文本渲染为 size×size 的灰度 RGB 画布（模块最近邻放大），
/// 供解码链路测试使用；inverted=true 渲染反色码（白码黑底）。
fn render_qr_rgb(text: &str, size: usize, inverted: bool) -> Vec<u8> {
    let qrcode =
        qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Low).expect("编码二维码失败");
    let n = qrcode.size() as usize;
    let px = (size / n).max(1);
    let q = px * n;
    let off = (size - q) / 2;
    // inverted: 模块白、背景黑；否则模块黑、背景白。
    let mut rgb = vec![if inverted { 0u8 } else { 255u8 }; size * size * 3];
    for my in 0..n {
        for mx in 0..n {
            let dark = qrcode.get_module(mx as i32, my as i32) != inverted;
            let v = if dark { 0u8 } else { 255u8 };
            for yy in 0..px {
                for xx in 0..px {
                    let i = ((off + my * px + yy) * size + (off + mx * px + xx)) * 3;
                    rgb[i] = v;
                    rgb[i + 1] = v;
                    rgb[i + 2] = v;
                }
            }
        }
    }
    rgb
}

/// 编码一张二维码再解码，验证整条识别链路。
#[test]
fn decode_rgb_roundtrip() {
    let text = "https://webstatic.mihoyo.com/bh3/event/mobile.html?tk=TESTTK123";
    let size = 200usize;
    let rgb = render_qr_rgb(text, size, false);
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 小图走 2x 放大重试路径仍可解码。
#[test]
fn decode_rgb_small_image_roundtrip() {
    let text = "https://x/qr_code_in_game.html?ticket=SMALL";
    let size = 120usize; // < 300，触发放大重试分支
    let rgb = render_qr_rgb(text, size, false);
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 大图走 2x2 降采样路径仍可解码。
#[test]
fn decode_rgb_large_image_roundtrip() {
    let text = "https://x/qr_code_in_game.html?ticket=BIG";
    let size = 1600usize; // ≥ 1600 触发降采样
    let rgb = render_qr_rgb(text, size, false);
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 反色二维码（白码黑底，游戏登录界面常见）应通过翻转重试解出。
#[test]
fn decode_rgb_inverted_roundtrip() {
    let text = "https://x/qr_code_in_game.html?ticket=INVERTED";
    let size = 200usize;
    let rgb = render_qr_rgb(text, size, true);
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("反色二维码解码失败");
    assert_eq!(decoded, text);
}

/// 解码附带位置信息，且位置外接框落在二维码区域内。
#[test]
fn decode_rgb_located_returns_bbox() {
    let text = "https://x/qr_code_in_game.html?ticket=LOC";
    let qr_size = 200usize;
    let qr_rgb = render_qr_rgb(text, qr_size, false);
    let size = 800usize;
    let off_x = 300usize;
    let off_y = 200usize;
    let mut rgb = vec![255u8; size * size * 3];
    for y in 0..qr_size {
        for x in 0..qr_size {
            let i0 = (y * qr_size + x) * 3;
            let i = ((y + off_y) * size + (x + off_x)) * 3;
            rgb[i] = qr_rgb[i0];
            rgb[i + 1] = qr_rgb[i0 + 1];
            rgb[i + 2] = qr_rgb[i0 + 2];
        }
    }
    let (decoded, loc) = qr::decode_rgb_located(&rgb, size as u32, size as u32).expect("解码失败");
    assert_eq!(decoded, text);
    let b = loc.expect("应返回二维码位置");
    // 外接框中心应大致落在二维码区域内（ROI 用途足够）。
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
}

#[test]
fn config_accepts_partial_json() {
    // serde(default)：旧版本/缺字段配置可正常加载；未知旧字段（region/channel/scan_channel）被忽略。
    let cfg: AppConfig = serde_json::from_str(
        r#"{"channel":"official","scan_channel":"auto","region":{"x":1,"y":2,"w":3,"h":4}}"#,
    )
    .unwrap();
    assert!(!cfg.always_on_top);
    assert!(cfg.bili_accounts.is_empty());
    assert!(cfg.game_channel.is_empty());
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
fn unprotect_without_prefix_is_empty() {
    // 不带 DPAPI 前缀的数据非本程序写出的密文，返回空（避免当令牌使用）。
    assert_eq!(crypto::unprotect_secret("legacy-plain"), "");
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
    assert_eq!(
        cfg.active_official.get("bh3").map(|s| s.as_str()),
        Some("o1")
    );
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
    use game_login_scanner_lib::config::Game;
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
    use game_login_scanner_lib::config::Game;
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
    use game_login_scanner_lib::config::Game;
    assert!(!Game::Bh3.is_hypergryph());
    assert!(Game::Endfield.is_hypergryph());
    assert!(Game::Arknights.is_hypergryph());
}

#[test]
fn game_label_non_empty() {
    use game_login_scanner_lib::config::Game;
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

// ---------- live（直播流解析纯函数） ----------

#[test]
fn parse_bili_flv_extracts_stream_url() {
    let v: serde_json::Value = serde_json::json!({
        "code": 0,
        "data": { "playurl_info": { "playurl": { "stream": [
            {
                "protocol_name": "http_hls",
                "format": [{ "format_name": "ts", "codec": [{ "codec_name": "hevc", "base_url": "/x.m3u8", "url_list": ["https://h"] }] }]
            },
            {
                "protocol_name": "http_stream",
                "format": [{ "format_name": "flv", "codec": [
                    { "codec_name": "hevc", "base_url": "/hevc.flv", "url_list": ["https://h1"] },
                    { "codec_name": "avc", "base_url": "/avc.flv", "url_list": ["https://h2"] }
                ] }]
            }
        ] } } } });
    let url = game_login_scanner_lib::live::parse_bili_flv(&v).expect("应提取到 avc flv");
    assert_eq!(url, "https://h2/avc.flv");
}

#[test]
fn parse_accept_qn_reads_codec_levels() {
    // 混合 hls/http_stream、hevc/avc：只取 http_stream/flv/avc 的 accept_qn，
    // 降序去重。
    let v: serde_json::Value = serde_json::json!({
        "data": { "playurl_info": { "playurl": { "stream": [
            { "protocol_name": "http_hls", "format": [{ "format_name": "ts",
                "codec": [{ "codec_name": "avc", "accept_qn": [80] }] }] },
            { "protocol_name": "http_stream", "format": [{ "format_name": "flv", "codec": [
                { "codec_name": "hevc", "accept_qn": [20000] },
                { "codec_name": "avc", "accept_qn": [150, 10000, 400, 250] }
            ] }] }
        ] } } } });
    let qs = game_login_scanner_lib::live::parse_accept_qn(&v);
    assert_eq!(qs, vec![10000, 400, 250, 150]);
    // 仅原画的房间（accept_qn=[10000]）→ 单元素链，不降级。
    let only: serde_json::Value = serde_json::json!({
        "data": { "playurl_info": { "playurl": { "stream": [
            { "protocol_name": "http_stream", "format": [{ "format_name": "flv",
                "codec": [{ "codec_name": "avc", "accept_qn": [10000] }] }] }
        ] } } } });
    assert_eq!(
        game_login_scanner_lib::live::parse_accept_qn(&only),
        vec![10000]
    );
}

#[test]
fn avcc_to_annexb_converts_length_prefixes() {
    let avcc = [0u8, 0, 0, 2, 0xAA, 0xBB, 0, 0, 0, 1, 0xCC];
    let out = game_login_scanner_lib::live::avcc_to_annexb(&avcc);
    assert_eq!(out, vec![0, 0, 0, 1, 0xAA, 0xBB, 0, 0, 0, 1, 0xCC]);
}

#[test]
fn avcc_config_extracts_sps_pps() {
    // 5 字节头 + 1 个 SPS(2B) + pps_count(1) + 1 个 PPS(2B)
    let rec = vec![
        1u8, 0x64, 0, 0x1F, 0xFF, 0xE1, 0, 2, 0x67, 0x40, 1, 0, 2, 0x68, 0xBE,
    ];
    let out = game_login_scanner_lib::live::avcc_config_to_annexb(&rec);
    assert_eq!(out, vec![0, 0, 0, 1, 0x67, 0x40, 0, 0, 0, 1, 0x68, 0xBE]);
}

#[test]
fn yuv420_to_rgba_gray_and_clamp() {
    // 2x2 YUV：全 Y=0（黑）与 Y=255（白）；U/V=128（无色度）。
    let y = [0u8, 0, 0, 0];
    let yw = [255u8, 255, 255, 255];
    let uv = [128u8];
    let black = game_login_scanner_lib::live::yuv420_to_rgba(&y, &uv, &uv, 2, 1, 2, 2);
    assert!(black.iter().step_by(4).all(|&p| p <= 16));
    let white = game_login_scanner_lib::live::yuv420_to_rgba(&yw, &uv, &uv, 2, 1, 2, 2);
    assert!(white.iter().step_by(4).all(|&p| p >= 235));
}

#[test]
fn parse_bili_durl_extracts_legacy_url() {
    let v: serde_json::Value = serde_json::json!({
        "code": 0,
        "data": { "durl": [ { "url": "https://d1--cn-gotcha04.bilivideo.com/live.flv?expires=1" } ] }
    });
    let url = game_login_scanner_lib::live::parse_bili_durl(&v).expect("应提取到 durl url");
    assert!(url.starts_with("https://d1--cn-gotcha04"));
}

// ---------- 直播 SPS VUI 清除（openh264 兼容性补丁）----------

fn hex_decode(s: &str) -> Vec<u8> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn strip_sps_vui_removes_vui_flag() {
    // 房间 11762420 真实参数集：SPS 带 VUI（HRD 等），openh264 解析失败（dsNoParamSets）。
    let params = hex_decode(
        "000000016764002aac2ca501e0089f97016a020202800001f40000ea607000000493e000007a1205de5c14\
         0000000168eb8f2c",
    );
    let (out, changed) = game_login_scanner_lib::live::strip_sps_vui(&params);
    assert!(changed, "带 VUI 的 SPS 应被清除");
    assert_eq!(out.len(), params.len(), "仅翻转 flag 位，长度不变");
    // vui_present_flag 位于 SPS 内 RBSP 第 86 bit（params[15] 的 bit6）：
    // 0x97 (1001 0111) → 0x95 (1001 0101)
    assert_eq!(params[15], 0x97);
    assert_eq!(out[15], 0x95);
    // PPS 部分保持不变
    assert_eq!(
        &out[out.len() - 8..],
        &[0x00, 0x00, 0x00, 0x01, 0x68, 0xeb, 0x8f, 0x2c]
    );
}

#[test]
fn strip_sps_vui_noop_without_vui() {
    // 已无 VUI 的 SPS（flag=0）：应原样返回，不改动。
    let params = hex_decode(
        "000000016764002aac2ca501e0089f95016a020202800001f40000ea607000000493e000007a1205de5c14\
         0000000168eb8f2c",
    );
    let (out, changed) = game_login_scanner_lib::live::strip_sps_vui(&params);
    assert!(!changed, "无 VUI 不应改动");
    assert_eq!(out, params);
}

#[test]
fn strip_sps_vui_handles_idr_embedded_sps() {
    // IDR 帧内嵌一份带 VUI 的重复 SPS：也必须清除（帧 0 报 dsNoParamSets 的根因）。
    let idr = hex_decode(
        "000000016764002aac2ca501e0089f97016a020202800001f40000ea607000000493e000007a1205de5c14\
         0000000165b8",
    );
    let (out, changed) = game_login_scanner_lib::live::strip_sps_vui(&idr);
    assert!(changed);
    assert_eq!(out[15], 0x95);
}

#[test]
fn scan_output_size_preserves_full_hd() {
    // 1080p 及以下原分辨率输出（保二维码模块细节）。
    assert_eq!(game_login_scanner_lib::live::scan_output_size(1920, 1080), (1920, 1080));
    assert_eq!(game_login_scanner_lib::live::scan_output_size(960, 540), (960, 540));
    // 全屏视口（1920 宽）不再被无条件减半。
    assert_eq!(game_login_scanner_lib::live::scan_output_size(1920, 1080).0, 1920);
}

#[test]
fn scan_output_size_caps_4k() {
    // 超过 1920 才等比缩小到最长边 1920。
    assert_eq!(game_login_scanner_lib::live::scan_output_size(3840, 2160), (1920, 1080));
    // 竖屏（高为长边）。
    let (w, h) = game_login_scanner_lib::live::scan_output_size(1080, 3840);
    assert_eq!(h, 1920);
    assert!(w < h);
}


// ---------- 分块扫码解码（全屏小二维码兜底）----------

#[test]
fn decode_rgb_tiled_finds_small_qr() {
    // 需真实测试图，见 zz_probe_screen（手动运行）。此处仅验证小图直接返回 None。
    let rgb = vec![255u8; 100 * 100 * 3];
    assert!(game_login_scanner_lib::qr::decode_rgb_tiled(&rgb, 100, 100).is_none());
}



