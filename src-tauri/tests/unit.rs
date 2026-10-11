//! 单元测试集合（原 lib 内联测试迁移至此，见 build.rs / Cargo.toml 中
//! 关于 Windows 测试 manifest 的说明）。

use game_login_scanner_lib::config::{AppConfig, BiliToken, Channel, OfficialToken};
use game_login_scanner_lib::constants;
use game_login_scanner_lib::crypto;
use game_login_scanner_lib::qr::{self, QrKind};
use game_login_scanner_lib::scan;
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
/// 供解码链路测试使用。白底黑码（所有游戏登录码均为正常极性）。
fn render_qr_rgb(text: &str, size: usize) -> Vec<u8> {
    let qrcode =
        qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Low).expect("编码二维码失败");
    let n = qrcode.size() as usize;
    let px = (size / n).max(1);
    let q = px * n;
    let off = (size - q) / 2;
    let mut rgb = vec![255u8; size * size * 3];
    for my in 0..n {
        for mx in 0..n {
            let dark = qrcode.get_module(mx as i32, my as i32);
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
    let rgb = render_qr_rgb(text, size);
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 小图走 2x 放大重试路径仍可解码。
#[test]
fn decode_rgb_small_image_roundtrip() {
    let text = "https://x/qr_code_in_game.html?ticket=SMALL";
    let size = 120usize; // < 300，触发放大重试分支
    let rgb = render_qr_rgb(text, size);
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 大图走 2x2 降采样路径仍可解码。
#[test]
fn decode_rgb_large_image_roundtrip() {
    let text = "https://x/qr_code_in_game.html?ticket=BIG";
    let size = 1600usize; // ≥ 1600 触发降采样
    let rgb = render_qr_rgb(text, size);
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 二维码在画布中偏移放置，仍应能整图解码。
#[test]
fn decode_rgb_offset_qr() {
    let text = "https://x/qr_code_in_game.html?ticket=LOC";
    let qr_size = 200usize;
    let qr_rgb = render_qr_rgb(text, qr_size);
    let size = 800usize;
    let (off_x, off_y) = (300usize, 200usize);
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
    let decoded = qr::decode_rgb(&rgb, size as u32, size as u32).expect("解码失败");
    assert_eq!(decoded, text);
}

/// decode_rgb_hit 应返回二维码在图像中的包围盒（用于 ROI 加速）。
#[test]
fn decode_rgb_hit_returns_bounds() {
    let text = "https://x/qr_code_in_game.html?ticket=ROI";
    let qr_size = 200usize;
    let qr_rgb = render_qr_rgb(text, qr_size);
    let size = 800usize;
    let (off_x, off_y) = (300i32, 200i32);
    let mut rgb = vec![255u8; size * size * 3];
    for y in 0..qr_size {
        for x in 0..qr_size {
            let i0 = (y * qr_size + x) * 3;
            let i = ((y + off_y as usize) * size + (x + off_x as usize)) * 3;
            rgb[i] = qr_rgb[i0];
            rgb[i + 1] = qr_rgb[i0 + 1];
            rgb[i + 2] = qr_rgb[i0 + 2];
        }
    }
    let hit = qr::decode_rgb_hit(&rgb, size as u32, size as u32).expect("解码失败");
    assert_eq!(hit.text, text);
    let (bx, by, bw, bh) = hit.bounds.expect("应返回包围盒");
    // 包围盒应大致覆盖二维码区域（允许 zedbar 记录的点略内缩，但不得偏出）。
    assert!(bx >= off_x - 40 && bx <= off_x + 40, "bx={bx} 偏离 {off_x}");
    assert!(by >= off_y - 40 && by <= off_y + 40, "by={by} 偏离 {off_y}");
    assert!(
        bw >= qr_size as u32 / 2 && bw <= (qr_size as u32) * 2,
        "bw={bw} 异常"
    );
    assert!(
        bh >= qr_size as u32 / 2 && bh <= (qr_size as u32) * 2,
        "bh={bh} 异常"
    );
}

// ---------- scan::crop_region（ROI 裁剪） ----------

/// 构造 width×height 的纯色 RGB 缓冲。
fn solid_rgb(w: usize, h: usize, c: [u8; 3]) -> Vec<u8> {
    let mut v = vec![0u8; w * h * 3];
    for px in v.as_chunks_mut::<3>().0 {
        px.copy_from_slice(&c);
    }
    v
}

#[test]
fn crop_region_normal() {
    let rgb = solid_rgb(200, 160, [10, 20, 30]);
    let (sub, w, h) = scan::crop_region(&rgb, 200, 160, 10, 20, 100, 90).unwrap();
    assert_eq!((w, h), (100, 90));
    assert_eq!(sub.len(), 100 * 90 * 3);
    assert_eq!(&sub[0..3], &[10, 20, 30]);
}

/// 负坐标应收敛到 0，且裁剪尺寸相应缩小（ROI 外扩可能越过左上角）。
#[test]
fn crop_region_negative_coords_clamped() {
    let rgb = solid_rgb(200, 160, [1, 2, 3]);
    // 请求 x=-20,y=-10,w=120,h=100 → 实际 x0=0,y0=0,x1=100,y1=90 → 100×90
    let (sub, w, h) = scan::crop_region(&rgb, 200, 160, -20, -10, 120, 100).unwrap();
    assert_eq!((w, h), (100, 90));
    assert_eq!(sub.len(), 100 * 90 * 3);
}

/// 超出右下边界应收敛到帧内。
#[test]
fn crop_region_overrun_clamped() {
    let rgb = solid_rgb(200, 160, [1, 2, 3]);
    // 请求 x=150,y=110,w=100,h=90 → x1=200,y1=160 → 50×50
    let (sub, w, h) = scan::crop_region(&rgb, 200, 160, 150, 110, 100, 90).unwrap();
    assert_eq!((w, h), (50, 50));
    assert_eq!(sub.len(), 50 * 50 * 3);
}

/// 裁剪后过小（任一边 < ROI_MIN_SIDE=32）应返回 None，退回全屏解码。
#[test]
fn crop_region_too_small_returns_none() {
    let rgb = solid_rgb(100, 80, [1, 2, 3]);
    assert!(scan::crop_region(&rgb, 100, 80, 0, 0, 10, 10).is_none());
    // 部分越界后过小
    assert!(scan::crop_region(&rgb, 100, 80, 95, 75, 50, 50).is_none());
}

/// 完全越界应返回 None。
#[test]
fn crop_region_fully_out_of_bounds() {
    let rgb = solid_rgb(100, 80, [1, 2, 3]);
    assert!(scan::crop_region(&rgb, 100, 80, 200, 200, 50, 50).is_none());
}

/// 裁剪子图的内容应逐像素对应原图（验证行跨度按原图宽度取值）。
#[test]
fn crop_region_preserves_pixels() {
    // 200×160 图，每像素编码 (x,y)，裁剪后逐点校验。
    let (w0, h0) = (200usize, 160usize);
    let mut rgb = vec![0u8; w0 * h0 * 3];
    for y in 0..h0 {
        for x in 0..w0 {
            let i = (y * w0 + x) * 3;
            rgb[i] = (x % 256) as u8;
            rgb[i + 1] = (y % 256) as u8;
            rgb[i + 2] = 0;
        }
    }
    let (x0, y0, cw, ch) = (10i32, 20i32, 100u32, 90u32);
    let (sub, w, h) = scan::crop_region(&rgb, w0 as u32, h0 as u32, x0, y0, cw, ch).unwrap();
    assert_eq!((w, h), (cw, ch));
    for yy in 0..ch as usize {
        for xx in 0..cw as usize {
            let si = (yy * cw as usize + xx) * 3;
            let ex = x0 as usize + xx;
            let ey = y0 as usize + yy;
            assert_eq!(sub[si], (ex % 256) as u8, "x 不匹配 @({ex},{ey})");
            assert_eq!(sub[si + 1], (ey % 256) as u8, "y 不匹配 @({ex},{ey})");
        }
    }
}

// ---------- ROI 端到端：bounds → 裁剪 → 子图解码 → 坐标翻译 ----------

/// 模拟 scan.rs 的 ROI 加速流程：全屏解码拿 bounds，外扩 ROI_PAD 裁剪子图，
/// 对子图解码并验证坐标翻译回全屏坐标系正确。
/// 这是 ROI 功能的核心假设——子图仍能解码且位置可还原。
#[test]
fn roi_crop_then_decode_roundtrip() {
    const ROI_PAD: i32 = 24;
    let text = "https://x/qr_code_in_game.html?ticket=ROIROUNDTRIP";
    let qr_size = 240usize;
    let qr_rgb = render_qr_rgb(text, qr_size);
    let size = 1000usize;
    let (off_x, off_y) = (380usize, 300usize);
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

    // 第一步：全屏解码，拿到 bounds。
    let full = qr::decode_rgb_hit(&rgb, size as u32, size as u32).expect("全屏解码失败");
    assert_eq!(full.text, text);
    let (fx, fy, fw, fh) = full.bounds.expect("全屏应返回 bounds");

    // 第二步：按 scan.rs 的规则外扩 ROI_PAD 裁剪子图。
    let rx = (fx - ROI_PAD).max(0);
    let ry = (fy - ROI_PAD).max(0);
    let pad2 = (ROI_PAD * 2) as u32;
    let (sub, sw, sh) =
        scan::crop_region(&rgb, size as u32, size as u32, rx, ry, fw + pad2, fh + pad2)
            .expect("ROI 裁剪失败");

    // 第三步：子图解码——ROI 功能的关键假设，必须能解出。
    let sub_hit = qr::decode_rgb_hit(&sub, sw, sh).expect("ROI 子图解码失败（静区不足？）");
    assert_eq!(sub_hit.text, text, "子图解码文本不一致");

    // 第四步：坐标翻译回全屏，应与原始 bounds 接近。
    let (sx, sy, _, _) = sub_hit.bounds.expect("子图应返回 bounds");
    let gx = sx + rx;
    let gy = sy + ry;
    assert!(
        (gx - fx).abs() <= 40,
        "翻译后 x 偏移过大：全屏 {fx} vs ROI {gx}（rx={rx}, sx={sx}）"
    );
    assert!(
        (gy - fy).abs() <= 40,
        "翻译后 y 偏移过大：全屏 {fy} vs ROI {gy}（ry={ry}, sy={sy}）"
    );
}

/// 严格探测：二维码紧贴干扰背景（无白边静区），ROI_PAD=24 固定外扩是否足够。
/// 真实游戏截图的二维码周围是游戏背景而非纯白，此测试还原该场景。
#[test]
fn roi_crop_with_noisy_background() {
    const ROI_PAD: i32 = 24;
    let text = "https://x/qr_code_in_game.html?ticket=NOISY";
    let qr_size = 300usize;
    let qr_rgb = render_qr_rgb(text, qr_size);
    let size = 1000usize;
    let (off_x, off_y) = (300usize, 300usize);
    // 背景：非白（深灰），二维码直接贴上，无额外静区。
    let mut rgb = vec![64u8; size * size * 3];
    for y in 0..qr_size {
        for x in 0..qr_size {
            let i0 = (y * qr_size + x) * 3;
            let i = ((y + off_y) * size + (x + off_x)) * 3;
            rgb[i] = qr_rgb[i0];
            rgb[i + 1] = qr_rgb[i0 + 1];
            rgb[i + 2] = qr_rgb[i0 + 2];
        }
    }
    // 全屏解码应成功（render 的 qr 自带白边）。
    let full = qr::decode_rgb_hit(&rgb, size as u32, size as u32).expect("全屏解码失败");
    assert_eq!(full.text, text);
    let (fx, fy, fw, fh) = full.bounds.expect("应返回 bounds");
    let rx = (fx - ROI_PAD).max(0);
    let ry = (fy - ROI_PAD).max(0);
    let pad2 = (ROI_PAD * 2) as u32;
    let (sub, sw, sh) =
        scan::crop_region(&rgb, size as u32, size as u32, rx, ry, fw + pad2, fh + pad2)
            .expect("ROI 裁剪失败");
    let hit = qr::decode_rgb_hit(&sub, sw, sh);
    assert!(
        hit.is_some(),
        "干扰背景下 ROI 解码失败（bounds {fw}×{fh}，外扩 {ROI_PAD}px 不足）——需按码尺寸比例外扩"
    );
}

/// ROI_PAD=24 对大二维码是否足够？zedbar 需要约 4 模块静区，
/// 大码单模块像素多，24px 可能不足——此测试探测该边界。
#[test]
fn roi_crop_large_qr() {
    const ROI_PAD: i32 = 24;
    let text = "https://x/qr_code_in_game.html?ticket=LARGEROI";
    let qr_size = 520usize; // 大码：单模块像素多
    let qr_rgb = render_qr_rgb(text, qr_size);
    let size = 1400usize;
    let (off_x, off_y) = (420usize, 400usize);
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
    let full = qr::decode_rgb_hit(&rgb, size as u32, size as u32).expect("全屏解码失败");
    assert_eq!(full.text, text);
    let (fx, fy, fw, fh) = full.bounds.expect("应返回 bounds");
    let rx = (fx - ROI_PAD).max(0);
    let ry = (fy - ROI_PAD).max(0);
    let pad2 = (ROI_PAD * 2) as u32;
    let (sub, sw, sh) =
        scan::crop_region(&rgb, size as u32, size as u32, rx, ry, fw + pad2, fh + pad2)
            .expect("ROI 裁剪失败");
    let hit = qr::decode_rgb_hit(&sub, sw, sh);
    assert!(
        hit.is_some(),
        "大码 {qr_size}px 在 ROI_PAD={ROI_PAD} 下 ROI 解码失败——静区不足，需增大 ROI_PAD 或按码尺寸外扩"
    );
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

// ---------- hls（LL-HLS 解析纯函数） ----------

#[test]
fn parse_m3u8_reads_segments_and_keyframe() {
    let m3u8 = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:100\n#EXT-X-MAP:URI=\"init.m4s\"\n#EXT-BILI-AUX:1|K|100|aa\n#EXTINF:1.0,\nseg1.m4s\n#EXT-BILI-AUX:2|N|100|bb\n#EXTINF:1.0,\nseg2.m4s\n";
    let pl = game_login_scanner_lib::hls::parse_m3u8(m3u8, "https://h/live/index.m3u8?x=1")
        .expect("m3u8 应解析成功");
    assert_eq!(pl.media_sequence, 100);
    assert_eq!(pl.target_duration, 2, "应读取 EXT-X-TARGETDURATION");
    assert_eq!(pl.segments.len(), 2);
    assert_eq!(pl.segments[0].seq, 100);
    assert!(pl.segments[0].keyframe, "首段应为关键帧");
    assert!(!pl.segments[1].keyframe);
    assert_eq!(
        pl.init_url.as_deref(),
        Some("https://h/live/init.m4s?x=1"),
        "相对 URI 应解析为绝对 URL 且保留 query"
    );

    // 缺失 EXT-X-TARGETDURATION 时默认 1。
    let pl2 = game_login_scanner_lib::hls::parse_m3u8(
        "#EXTM3U\n#EXTINF:1.0,\na.m4s\n",
        "https://h/live/index.m3u8",
    )
    .expect("应解析成功");
    assert_eq!(pl2.target_duration, 1);
}

#[test]
fn hls_parse_init_reads_video_track_id() {
    // 最小 init 段：视频轨（含 avc1）的 track_id 故意设为 2。
    // 修复前代码硬编码 track_id=1，会按错误的 id 取到音频轨、解码 0 帧。
    let hex = "000000146674797069736f6d0000000069736f6d000000fb6d6f6f76000000f37472616b00000034746b68640000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000b76d646961000000206d64686400000000000000000000000000015f9000000000000000000000008f6d696e66000000877374626c0000007f7374736400000000000000010000006f61766331000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000196176634301640028ffe100046764002801000268eb";
    let data: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    let info = game_login_scanner_lib::hls::parse_init(&data).expect("init 应解析成功");
    assert_eq!(info.track_id, 2, "应读出视频轨 track_id=2（而非硬编码 1）");
    assert_eq!(info.timescale, 90000);
    assert!(!info.params.is_empty(), "应提取到 SPS/PPS");
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
fn parse_bili_flv_reads_url_info() {
    // B站现行 API：url_list 为 null，改用 url_info[{host, extra}]。
    // 完整 URL = host + base_url + extra。
    let v: serde_json::Value = serde_json::json!({
        "data": { "playurl_info": { "playurl": { "stream": [
            {
                "protocol_name": "http_stream",
                "format": [{ "format_name": "flv", "codec": [{
                    "codec_name": "avc",
                    "base_url": "/live.flv?",
                    "url_list": serde_json::Value::Null,
                    "url_info": [{ "host": "https://d1--cn.bilivideo.com", "extra": "expires=123&qn=250" }]
                }] }]
            }
        ] } } } });
    let url = game_login_scanner_lib::live::parse_bili_flv(&v).expect("应提取到 flv");
    assert_eq!(
        url,
        "https://d1--cn.bilivideo.com/live.flv?expires=123&qn=250"
    );
}

#[test]
fn parse_bili_flv_ignores_hls_only() {
    // 仅有 HLS 流的房间（无 http_stream/flv）：返回 None，由上层回退/报错。
    let v: serde_json::Value = serde_json::json!({
        "data": { "playurl_info": { "playurl": { "stream": [
            {
                "protocol_name": "http_hls",
                "format": [{ "format_name": "fmp4", "codec": [{
                    "codec_name": "avc", "base_url": "/index.m3u8?",
                    "url_info": [{ "host": "https://h", "extra": "e=1" }]
                }] }]
            }
        ] } } } });
    assert!(game_login_scanner_lib::live::parse_bili_flv(&v).is_none());
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
    assert_eq!(
        game_login_scanner_lib::live::scan_output_size(1920, 1080),
        (1920, 1080)
    );
    assert_eq!(
        game_login_scanner_lib::live::scan_output_size(960, 540),
        (960, 540)
    );
    // 全屏视口（1920 宽）不再被无条件减半。
    assert_eq!(
        game_login_scanner_lib::live::scan_output_size(1920, 1080).0,
        1920
    );
}

#[test]
fn scan_output_size_caps_4k() {
    // 超过 1920 才等比缩小到最长边 1920。
    assert_eq!(
        game_login_scanner_lib::live::scan_output_size(3840, 2160),
        (1920, 1080)
    );
    // 竖屏（高为长边）。
    let (w, h) = game_login_scanner_lib::live::scan_output_size(1080, 3840);
    assert_eq!(h, 1920);
    assert!(w < h);
}

// ---------- 扫码解码（zedbar）----------

#[test]
fn decode_rgb_rejects_blank() {
    // 纯色图无二维码，应返回 None（不 panic）。
    let rgb = vec![255u8; 200 * 200 * 3];
    assert!(game_login_scanner_lib::qr::decode_rgb(&rgb, 200, 200).is_none());
    // 空尺寸安全返回。
    assert!(game_login_scanner_lib::qr::decode_rgb(&rgb, 0, 0).is_none());
}

// ---------- config::display_fps（显示区域帧率设置）----------

#[test]
fn display_fps_normalized_to_whitelist() {
    use game_login_scanner_lib::config::normalize_display_fps;
    // 仅 30/60/120 有效；其它值（含手改配置）收敛到 60。
    assert_eq!(normalize_display_fps(30), 30);
    assert_eq!(normalize_display_fps(60), 60);
    assert_eq!(normalize_display_fps(120), 120);
    assert_eq!(normalize_display_fps(0), 60);
    assert_eq!(normalize_display_fps(15), 60);
    assert_eq!(normalize_display_fps(240), 60);
}

#[test]
fn display_fps_defaults_to_60_for_old_config() {
    // 旧配置无 display_fps 字段：serde 默认 60。
    let cfg: AppConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(cfg.display_fps, 60);
    // 显式写入也能读回。
    let cfg2: AppConfig = serde_json::from_str(r#"{"display_fps":120}"#).unwrap();
    assert_eq!(cfg2.display_fps, 120);
}

// ---------- http 解析层（mock：本地内存字节，不触网） ----------

#[test]
fn http_split_url_forms() {
    use game_login_scanner_lib::http::split_url;
    // https 默认端口
    assert_eq!(
        split_url("https://a.com/p?q=1").unwrap(),
        (true, "a.com".to_string(), 443, "/p?q=1".to_string())
    );
    // 显式端口 + 无路径
    assert_eq!(
        split_url("https://a.com:8443").unwrap(),
        (true, "a.com".to_string(), 8443, "/".to_string())
    );
    // http 默认端口
    assert_eq!(
        split_url("http://a.com/x").unwrap(),
        (false, "a.com".to_string(), 80, "/x".to_string())
    );
    // 非法协议
    assert!(split_url("ftp://a.com").is_err());
    // 缺主机名
    assert!(split_url("https:///path").is_err());
}

#[test]
fn http_parse_response_status_and_headers() {
    use game_login_scanner_lib::http::{header_find, parse_response};
    let raw = b"HTTP/1.1 302 Found\r\nLocation: https://b.com/x\r\nContent-Length: 0\r\n\r\n";
    let (status, headers) = parse_response(raw).unwrap();
    assert_eq!(status, 302);
    // 头名大小写不敏感查找
    assert_eq!(header_find(&headers, "location"), Some("https://b.com/x"));
    assert_eq!(header_find(&headers, "LOCATION"), Some("https://b.com/x"));
    assert_eq!(header_find(&headers, "missing"), None);
}

#[test]
fn http_parse_response_rejects_bad_status() {
    use game_login_scanner_lib::http::parse_response;
    assert!(parse_response(b"garbage\r\n").is_err());
}

#[test]
fn http_read_body_content_length() {
    use game_login_scanner_lib::http::read_body;
    let headers = vec![("Content-Length".to_string(), "5".to_string())];
    let mut stream = std::io::Cursor::new(b"hello world".to_vec());
    let body = read_body(&mut stream, &headers, Vec::new()).unwrap();
    assert_eq!(body, b"hello");
}

#[test]
fn http_read_body_chunked() {
    use game_login_scanner_lib::http::read_body;
    let headers = vec![("Transfer-Encoding".to_string(), "chunked".to_string())];
    // 5 字节 + 6 字节两块，末尾 0 块
    let raw = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n".to_vec();
    let mut stream = std::io::Cursor::new(raw);
    let body = read_body(&mut stream, &headers, Vec::new()).unwrap();
    assert_eq!(body, b"hello world");
}

#[test]
fn http_read_body_chunked_with_prefix() {
    use game_login_scanner_lib::http::read_body;
    let headers = vec![("Transfer-Encoding".to_string(), "chunked".to_string())];
    // prefix 已含部分 body 字节（响应头后多读），应与之拼接后正确解帧。
    let prefix = b"3\r\nabc".to_vec();
    let rest = b"\r\n0\r\n\r\n".to_vec();
    let mut stream = std::io::Cursor::new(rest);
    let body = read_body(&mut stream, &headers, prefix).unwrap();
    assert_eq!(body, b"abc");
}

#[test]
fn http_read_body_to_eof_without_framing() {
    use game_login_scanner_lib::http::read_body;
    // 无 Content-Length / chunked：读到 EOF。
    let headers: Vec<(String, String)> = Vec::new();
    let mut stream = std::io::Cursor::new(b"abcdef".to_vec());
    let body = read_body(&mut stream, &headers, Vec::new()).unwrap();
    assert_eq!(body, b"abcdef");
}
