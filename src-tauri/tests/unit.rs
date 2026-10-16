//! 单元测试集合（原 lib 内联测试迁移至此，见 build.rs / Cargo.toml 中
//! 关于 Windows 测试 manifest 的说明）。
//! 注：本文件与 smoke.rs 随仓库入库并由 CI 运行；依赖 tests/assets 的用例
//! 在素材缺失时自动跳过，不阻断 CI。

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

/// RGB（BT.601，与生产取帧侧同权重）→ 灰度。
fn rgb_to_gray_t(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    let n = w as usize * h as usize;
    rgb.as_chunks::<3>()
        .0
        .iter()
        .take(n)
        .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 151 + p[2] as u32 * 28) >> 8) as u8)
        .collect()
}

/// RGB 画布 → 灰度解码（生产只有灰度入口，测试在此做一次转换）。
fn decode_rgb_hit_t(rgb: &[u8], w: u32, h: u32) -> Option<qr::QrHit> {
    qr::decode_gray_hit(&rgb_to_gray_t(rgb, w, h), w, h)
}

fn decode_rgb_t(rgb: &[u8], w: u32, h: u32) -> Option<String> {
    decode_rgb_hit_t(rgb, w, h).map(|hit| hit.text)
}

#[test]
fn rgba_to_gray_weights() {
    // 纯红 / 纯绿 的 BT.601 亮度（(77R+151G+28B)>>8）。
    let rgba = [255, 0, 0, 128, 0, 255, 0, 128];
    assert_eq!(qr::rgba_to_gray(&rgba, 2, 1), vec![76, 150]);
}

#[test]
fn decode_gray_empty() {
    assert!(qr::decode_gray_hit(&[], 0, 0).is_none());
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
/// 小二维码（大画布中）必须能被识别——覆盖「有界小码重试」兜底路径。
///
/// 实测（1000×1000 画布）：60/80px 的码**主扫描（无重试）解不出**，靠有界重试
/// （候选区域裁剪 + 2x/4x 放大重扫）成功；本测试即锁定该能力不回退。
#[test]
fn decode_small_qr_in_large_canvas() {
    let text = "https://x/qr_code_in_game.html?ticket=SMALLQR";
    for qr_size in [70usize, 100] {
        let qr_rgb = render_qr_rgb(text, qr_size);
        let size = 1000usize;
        let (off_x, off_y) = (450usize, 460usize);
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
        let gray = rgb_to_gray_t(&rgb, size as u32, size as u32);
        // 保真与快路径都必须能识别（生产直播路径用快路径，屏幕路径两者皆用）。
        let exact = qr::decode_gray_hit(&gray, size as u32, size as u32);
        assert!(exact.is_some(), "{qr_size}px 小码：保真路径未识别");
        assert_eq!(exact.unwrap().text, text);
        let fast = qr::decode_gray_hit_fast(&gray, size as u32, size as u32);
        assert!(
            fast.is_some(),
            "{qr_size}px 小码：快路径未识别（有界重试失效？）"
        );
        assert_eq!(fast.unwrap().text, text);
    }
}

#[test]
fn decode_rgb_roundtrip() {
    let text = "https://webstatic.mihoyo.com/bh3/event/mobile.html?tk=TESTTK123";
    let size = 200usize;
    let rgb = render_qr_rgb(text, size);
    let decoded = decode_rgb_t(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 小图（码占满整帧）仍可解码；小码兜底已改为有界重试（见 bounded_retry）。
#[test]
fn decode_rgb_small_image_roundtrip() {
    let text = "https://x/qr_code_in_game.html?ticket=SMALL";
    let size = 120usize; // < 300，触发放大重试分支
    let rgb = render_qr_rgb(text, size);
    let decoded = decode_rgb_t(&rgb, size as u32, size as u32).expect("解码二维码失败");
    assert_eq!(decoded, text);
}

/// 大图走 2x2 降采样路径仍可解码。
#[test]
fn decode_rgb_large_image_roundtrip() {
    let text = "https://x/qr_code_in_game.html?ticket=BIG";
    let size = 1600usize; // ≥ 1600 触发降采样
    let rgb = render_qr_rgb(text, size);
    let decoded = decode_rgb_t(&rgb, size as u32, size as u32).expect("解码二维码失败");
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
    let decoded = decode_rgb_t(&rgb, size as u32, size as u32).expect("解码失败");
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
    let hit = decode_rgb_hit_t(&rgb, size as u32, size as u32).expect("解码失败");
    assert_eq!(hit.text, text);
    let (bx, by, bw, bh) = hit.bounds.expect("应返回包围盒");
    // 包围盒应大致覆盖二维码区域（允许解码器记录的点略内缩，但不得偏出）。
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

// ---------- scan::crop_region（ROI 裁剪，灰度 1 字节/像素） ----------

/// 构造 width×height 的纯灰度缓冲。
fn solid_gray(w: usize, h: usize, v: u8) -> Vec<u8> {
    vec![v; w * h]
}

#[test]
fn crop_region_normal() {
    let gray = solid_gray(200, 160, 10);
    let (sub, w, h) = scan::crop_region(&gray, 200, 160, 10, 20, 100, 90).unwrap();
    assert_eq!((w, h), (100, 90));
    assert_eq!(sub.len(), 100 * 90);
    assert_eq!(sub[0], 10);
}

/// 负坐标应收敛到 0，且裁剪尺寸相应缩小（ROI 外扩可能越过左上角）。
#[test]
fn crop_region_negative_coords_clamped() {
    let gray = solid_gray(200, 160, 3);
    // 请求 x=-20,y=-10,w=120,h=100 → 实际 x0=0,y0=0,x1=100,y1=90 → 100×90
    let (sub, w, h) = scan::crop_region(&gray, 200, 160, -20, -10, 120, 100).unwrap();
    assert_eq!((w, h), (100, 90));
    assert_eq!(sub.len(), 100 * 90);
}

/// 超出右下边界应收敛到帧内。
#[test]
fn crop_region_overrun_clamped() {
    let gray = solid_gray(200, 160, 3);
    // 请求 x=150,y=110,w=100,h=90 → x1=200,y1=160 → 50×50
    let (sub, w, h) = scan::crop_region(&gray, 200, 160, 150, 110, 100, 90).unwrap();
    assert_eq!((w, h), (50, 50));
    assert_eq!(sub.len(), 50 * 50);
}

/// 裁剪后过小（任一边 < ROI_MIN_SIDE=32）应返回 None，退回全屏解码。
#[test]
fn crop_region_too_small_returns_none() {
    let gray = solid_gray(100, 80, 3);
    assert!(scan::crop_region(&gray, 100, 80, 0, 0, 10, 10).is_none());
    // 部分越界后过小
    assert!(scan::crop_region(&gray, 100, 80, 95, 75, 50, 50).is_none());
}

/// 完全越界应返回 None。
#[test]
fn crop_region_fully_out_of_bounds() {
    let gray = solid_gray(100, 80, 3);
    assert!(scan::crop_region(&gray, 100, 80, 200, 200, 50, 50).is_none());
}

/// 裁剪子图的内容应逐像素对应原图（验证行跨度按原图宽度取值）。
#[test]
fn crop_region_preserves_pixels() {
    // 200×160 图，每像素编码 (x,y) 的混合值，裁剪后逐点校验。
    let (w0, h0) = (200usize, 160usize);
    let mut gray = vec![0u8; w0 * h0];
    for y in 0..h0 {
        for x in 0..w0 {
            gray[y * w0 + x] = ((x * 7 + y * 13) % 256) as u8;
        }
    }
    let (x0, y0, cw, ch) = (10i32, 20i32, 100u32, 90u32);
    let (sub, w, h) = scan::crop_region(&gray, w0 as u32, h0 as u32, x0, y0, cw, ch).unwrap();
    assert_eq!((w, h), (cw, ch));
    for yy in 0..ch as usize {
        for xx in 0..cw as usize {
            let si = yy * cw as usize + xx;
            let ex = x0 as usize + xx;
            let ey = y0 as usize + yy;
            assert_eq!(
                sub[si],
                ((ex * 7 + ey * 13) % 256) as u8,
                "像素不匹配 @({ex},{ey})"
            );
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

    // 生产链路：RGB 源 → 灰度（scan.rs 直接产出灰度）→ 解码/裁剪。
    let gray = rgb_to_gray_t(&rgb, size as u32, size as u32);

    // 第一步：全屏解码，拿到 bounds。
    let full = qr::decode_gray_hit(&gray, size as u32, size as u32).expect("全屏解码失败");
    assert_eq!(full.text, text);
    let (fx, fy, fw, fh) = full.bounds.expect("全屏应返回 bounds");

    // 第二步：按 scan.rs 的规则外扩 ROI_PAD 裁剪子图。
    let rx = (fx - ROI_PAD).max(0);
    let ry = (fy - ROI_PAD).max(0);
    let pad2 = (ROI_PAD * 2) as u32;
    let (sub, sw, sh) = scan::crop_region(
        &gray,
        size as u32,
        size as u32,
        rx,
        ry,
        fw + pad2,
        fh + pad2,
    )
    .expect("ROI 裁剪失败");

    // 第三步：子图解码——ROI 功能的关键假设，必须能解出。
    let sub_hit = qr::decode_gray_hit(&sub, sw, sh).expect("ROI 子图解码失败（静区不足？）");
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
    let gray = rgb_to_gray_t(&rgb, size as u32, size as u32);
    // 全屏解码应成功（render 的 qr 自带白边）。
    let full = qr::decode_gray_hit(&gray, size as u32, size as u32).expect("全屏解码失败");
    assert_eq!(full.text, text);
    let (fx, fy, fw, fh) = full.bounds.expect("应返回 bounds");
    let rx = (fx - ROI_PAD).max(0);
    let ry = (fy - ROI_PAD).max(0);
    let pad2 = (ROI_PAD * 2) as u32;
    let (sub, sw, sh) = scan::crop_region(
        &gray,
        size as u32,
        size as u32,
        rx,
        ry,
        fw + pad2,
        fh + pad2,
    )
    .expect("ROI 裁剪失败");
    let hit = qr::decode_gray_hit(&sub, sw, sh);
    assert!(
        hit.is_some(),
        "干扰背景下 ROI 解码失败（bounds {fw}×{fh}，外扩 {ROI_PAD}px 不足）——需按码尺寸比例外扩"
    );
}

/// ROI_PAD=24 对大二维码是否足够？解码需要约 4 模块静区，
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
    let gray = rgb_to_gray_t(&rgb, size as u32, size as u32);
    let full = qr::decode_gray_hit(&gray, size as u32, size as u32).expect("全屏解码失败");
    assert_eq!(full.text, text);
    let (fx, fy, fw, fh) = full.bounds.expect("应返回 bounds");
    let rx = (fx - ROI_PAD).max(0);
    let ry = (fy - ROI_PAD).max(0);
    let pad2 = (ROI_PAD * 2) as u32;
    let (sub, sw, sh) = scan::crop_region(
        &gray,
        size as u32,
        size as u32,
        rx,
        ry,
        fw + pad2,
        fh + pad2,
    )
    .expect("ROI 裁剪失败");
    let hit = qr::decode_gray_hit(&sub, sw, sh);
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

#[test]
fn header_utf8_preserves_percent() {
    use game_login_scanner_lib::http::header_utf8;
    // 已是百分号编码的值（抖音 ttwid）不应被二次编码。
    assert_eq!(header_utf8("1%7Cabc%7C123"), "1%7Cabc%7C123");
    // 中文按 UTF-8 百分号编码。
    assert_eq!(header_utf8("中"), "%E4%B8%AD");
    // 纯 ASCII 原样。
    assert_eq!(header_utf8("Chrome/126.0"), "Chrome/126.0");
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
fn unprotect_without_prefix_returns_plain() {
    // 不带前缀 = 明文（旧版配置 / 未加密平台），原样返回以向后兼容。
    // 注：带前缀但解密失败的才返回空（避免把密文当令牌用）。
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

// ---------- 扫码解码（rxing）----------

#[test]
fn decode_rgb_rejects_blank() {
    // 纯色图无二维码，应返回 None（不 panic）。
    let rgb = vec![255u8; 200 * 200 * 3];
    assert!(decode_rgb_t(&rgb, 200, 200).is_none());
    // 空尺寸安全返回。
    assert!(decode_rgb_t(&rgb, 0, 0).is_none());
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

// ---------- live_history（多直播间阶段 1） ----------

#[test]
fn live_history_push_appends_and_keeps_order() {
    let mut cfg = AppConfig::default();
    for i in 1..=35 {
        cfg.push_live_history(&format!("10{:03}", i), "bilibili", ""); // 10001 ~ 10035
    }
    // 固定顺序 + 上限收敛到 30：新条目追加到末尾，超限淘汰最久未用的 5 个
    // （10001~10005），其余保持添加顺序。
    assert_eq!(cfg.live_history.len(), 30);
    assert_eq!(cfg.live_history[0].room, "10006");
    assert_eq!(cfg.live_history.last().unwrap().room, "10035");
    // 重复记录既有房间：只刷新使用时间，**位置不变**（不置顶）——列表顺序
    // 只允许用户拖拽变更。
    cfg.push_live_history("10010", "bilibili", "");
    assert_eq!(cfg.live_history.len(), 30);
    assert_eq!(cfg.live_history[4].room, "10010");
    // 备注语义：重复 push 且 note 为空时保留旧备注，有值时覆盖。
    cfg.push_live_history("10010", "bilibili", "主播A");
    assert_eq!(cfg.live_history[4].note, "主播A");
    cfg.push_live_history("10010", "bilibili", "");
    assert_eq!(cfg.live_history[4].note, "主播A");
}

/// 拖拽排序后：连接既有房间（push）不得改变顺序；新房间追加末尾；
/// 配置往返（重启）后顺序保持。
#[test]
fn live_history_reorder_then_push_keeps_order() {
    fn rooms(cfg: &AppConfig) -> Vec<String> {
        cfg.live_history.iter().map(|e| e.room.clone()).collect()
    }
    let mut cfg = AppConfig::default();
    for r in ["101", "102", "103"] {
        cfg.push_live_history(r, "bilibili", "");
    }
    // 拖拽：把 103 拖到最前。
    cfg.reorder_live_history(&[
        "bilibili:103".to_string(),
        "bilibili:101".to_string(),
        "bilibili:102".to_string(),
    ]);
    assert_eq!(rooms(&cfg), ["103", "101", "102"]);
    // 连接 101（已存在）：位置不变。
    cfg.push_live_history("101", "bilibili", "");
    assert_eq!(rooms(&cfg), ["103", "101", "102"]);
    // 新增 104：追加末尾，既有顺序不动。
    cfg.push_live_history("104", "bilibili", "");
    assert_eq!(rooms(&cfg), ["103", "101", "102", "104"]);
    // 保存/加载往返后顺序不变。
    let json = serde_json::to_string(&cfg).expect("序列化");
    let reloaded: AppConfig = serde_json::from_str(&json).expect("反序列化");
    assert_eq!(rooms(&reloaded), ["103", "101", "102", "104"]);
}

#[test]
fn live_history_remove() {
    let mut cfg = AppConfig::default();
    cfg.push_live_history("1001", "bilibili", "");
    cfg.push_live_history("1002", "bilibili", "");
    assert!(cfg.remove_live_history("1001", "bilibili"));
    assert!(!cfg.live_history.iter().any(|e| e.room == "1001"));
    // 不存在的房间：返回 false 且无副作用。
    assert!(!cfg.remove_live_history("9999", "bilibili"));
    assert_eq!(cfg.live_history.len(), 1);
}

#[test]
fn live_history_ignores_blank_room() {
    let mut cfg = AppConfig::default();
    cfg.push_live_history("   ", "bilibili", "");
    cfg.push_live_history("", "bilibili", "");
    assert!(cfg.live_history.is_empty());
}

#[test]
fn live_history_platform_distinguishes_same_room() {
    let mut cfg = AppConfig::default();
    // 同一房间号在 B站/抖音各存一条（不互相去重）。
    cfg.push_live_history("123", "bilibili", "");
    cfg.push_live_history("123", "douyin", "");
    assert_eq!(cfg.live_history.len(), 2);
    // 旧值 "bili" 归一到 "bilibili"：与已有 bilibili 记录去重。
    cfg.push_live_history("123", "bili", "");
    assert_eq!(cfg.live_history.len(), 2);
    assert!(cfg
        .live_history
        .iter()
        .all(|e| e.platform == "bilibili" || e.platform == "douyin"));
}

// ---------- 抖音直播流解析 ----------

#[test]
fn parse_douyin_flv_prefers_full_hd() {
    let v: serde_json::Value = serde_json::json!({
        "data": { "data": [ {
            "stream_url": {
                "flv_pull_url": {
                    "sd2": "http://pull-sd2.douyincdn.com/x.flv",
                    "hd1": "http://pull-hd1.douyincdn.com/x.flv",
                    "full_hd1": "http://pull-full.douyincdn.com/x.flv"
                }
            }
        } ] }
    });
    let url = game_login_scanner_lib::douyin::parse_douyin_flv(&v).expect("应提取 flv");
    // 明文 http 升级为 https；优先 full_hd1。
    assert_eq!(url, "https://pull-full.douyincdn.com/x.flv");
}

#[test]
fn parse_douyin_flv_fallback_any_key() {
    let v: serde_json::Value = serde_json::json!({
        "data": { "data": [ {
            "stream_url": { "flv_pull_url": { "custom": "https://c.douyincdn.com/y.flv" } }
        } ] }
    });
    let url = game_login_scanner_lib::douyin::parse_douyin_flv(&v).expect("应提取 flv");
    assert_eq!(url, "https://c.douyincdn.com/y.flv");
}

#[test]
fn parse_douyin_flv_reads_stream_data_json() {
    // 路径二：live_core_sdk_data.pull_data.stream_data（JSON 字符串）→ data.*.main.flv
    let stream_data = serde_json::json!({
        "data": { "origin": { "main": { "flv": "http://sd.douyincdn.com/z.flv" } } }
    })
    .to_string();
    let v: serde_json::Value = serde_json::json!({
        "data": { "data": [ {
            "stream_url": {
                "live_core_sdk_data": { "pull_data": { "stream_data": stream_data } }
            }
        } ] }
    });
    let url = game_login_scanner_lib::douyin::parse_douyin_flv(&v).expect("应提取 stream_data flv");
    assert_eq!(url, "https://sd.douyincdn.com/z.flv");
}

#[test]
fn parse_douyin_flv_none_when_absent() {
    let v: serde_json::Value = serde_json::json!({ "data": { "data": [ {} ] } });
    assert!(game_login_scanner_lib::douyin::parse_douyin_flv(&v).is_none());
    // 空 flv_pull_url 也不应误取。
    let v2: serde_json::Value = serde_json::json!({
        "data": { "data": [ { "stream_url": { "flv_pull_url": { "full_hd1": "" } } } ] }
    });
    assert!(game_login_scanner_lib::douyin::parse_douyin_flv(&v2).is_none());
}

#[test]
fn platform_parse_and_label() {
    use game_login_scanner_lib::live::Platform;
    assert_eq!(Platform::parse("douyin"), Platform::Douyin);
    assert_eq!(Platform::parse("bilibili"), Platform::Bilibili);
    // 未知/空 → 默认 B站（兼容旧调用）。
    assert_eq!(Platform::parse(""), Platform::Bilibili);
    assert_eq!(Platform::parse("weird"), Platform::Bilibili);
    assert_eq!(Platform::Douyin.label(), "抖音");
    assert_eq!(Platform::Bilibili.label(), "B站");
}

// ---------- 主档降采样：真实登录码的检出回归 ----------

/// 主档（屏幕 1/2 / 直播 1/2）降采样**必须**仍能检出真实游戏登录码。
///
/// 这是降采样方案的核心不变量。`tests/bench_qr.rs` 曾记「1/2 降采样两张真机
/// 截图均漏检、方案已否决」，2026-10-13 复核发现：漏检是**面积平均（box）**
/// 降采样抹平模块边缘对比度所致（HybridBinarizer 依赖局部对比）；改用
/// **步长采样**后两张真机截图均命中，故降采样方案复活（2569x1445 解码
/// 20.7ms → 6.2ms）。本测试把该结论固化为回归：换采样方式/换解码器时必须通过。
///
/// 素材缺失（tests/assets 未准备）时打印提示并跳过——不阻断 CI。
#[test]
fn half_stride_keeps_real_login_qr() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("跳过：无 tests/assets（先跑 scripts/prepare-bench-assets.ps1）");
        return;
    };
    let mut files: Vec<_> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rgb"))
        .collect();
    files.sort();
    if files.is_empty() {
        eprintln!("跳过：tests/assets 下没有 .rgb");
        return;
    }
    for path in files {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let Some(rest) = stem.strip_prefix("screen_") else {
            continue;
        };
        let Some((ws, hs)) = rest.split_once('x') else {
            continue;
        };
        let (Ok(w), Ok(h)) = (ws.parse::<u32>(), hs.parse::<u32>()) else {
            continue;
        };
        let Ok(mut data) = std::fs::read(&path) else {
            continue;
        };
        assert_eq!(data.len(), (w * h * 3) as usize, "{stem} 尺寸与文件名不符");
        // GDI 24bpp 是 BGR 内存序 → 交换 R/B。
        for px in data.as_chunks_mut::<3>().0 {
            px.swap(0, 2);
        }
        let gray: Vec<u8> = data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 151 + p[2] as u32 * 28) >> 8) as u8)
            .collect();
        // 全分辨率：必须检出（基线）。
        let base = qr::decode_gray_hit_fast(&gray, w, h);
        assert!(base.is_some(), "{stem}: 全分辨率基线未检出登录码");
        // 1/2 步长（生产主档）：必须检出。
        let k = 2u32;
        let (dw, dh) = (w / k, h / k);
        let mut small = Vec::with_capacity((dw * dh) as usize);
        for y in 0..dh {
            let row = (y * k) as usize * w as usize;
            for x in 0..dw {
                small.push(gray[row + (x * k) as usize]);
            }
        }
        // 1/2 步长（生产主档）必须与全分辨率一样命中。
        let hit = qr::decode_gray_hit_fast(&small, dw, dh);
        assert!(hit.is_some(), "{stem}: 1/2 步长降采样后未检出登录码");
    }
}

// ---------- duplication::bgra_gray_stride（屏幕取帧一趟转换，逐位等价回归）----------

/// `bgra_gray_stride`（BGRA→步长采样灰度，一趟）必须与「先全分辨率灰度、
/// 再按 scale 步长降采样」两趟结果**逐位一致**——这是主档（scale=2）合并转换
/// 的前提（同一源像素的同一亮度值），保证识别行为不变。
#[cfg(windows)]
#[test]
fn bgra_gray_stride_matches_two_pass_reference() {
    use game_login_scanner_lib::duplication::bgra_gray_stride;
    // 构造确定性 BGRA（含行尾 padding，验证 pitch 处理），宽高取非整除数。
    let (w, h, pitch) = (10usize, 6usize, 10usize * 4 + 8);
    let mut bgra = vec![0u8; pitch * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * pitch + x * 4;
            bgra[i] = ((x * 7 + y * 3) % 256) as u8; // B
            bgra[i + 1] = ((x * 5 + y * 11) % 256) as u8; // G
            bgra[i + 2] = ((x * 13 + y * 2) % 256) as u8; // R
            bgra[i + 3] = 255; // A
        }
    }
    // 两趟参考：先全分辨率灰度（cw×ch），再步长采样。
    let gray = |b: &[u8], cx: usize, cy: usize, cw: usize, ch: usize| -> Vec<u8> {
        let mut g = vec![0u8; cw * ch];
        for yy in 0..ch {
            for xx in 0..cw {
                let i = (cy + yy) * pitch + (cx + xx) * 4;
                g[yy * cw + xx] =
                    ((b[i + 2] as u32 * 77 + b[i + 1] as u32 * 151 + b[i] as u32 * 28) >> 8) as u8;
            }
        }
        g
    };
    for &scale in &[1usize, 2usize] {
        for &(cx, cy, cw, ch) in &[(0usize, 0usize, 10u32, 6u32), (2, 1, 6, 4), (1, 0, 9, 5)] {
            let (one, ow, oh) = bgra_gray_stride(&bgra, pitch, cx, cy, cw, ch, scale);
            let full = gray(&bgra, cx, cy, cw as usize, ch as usize);
            let (ow, oh) = (ow as usize, oh as usize);
            let mut two = vec![0u8; ow * oh];
            for oy in 0..oh {
                for ox in 0..ow {
                    two[oy * ow + ox] = full[(oy * scale) * cw as usize + ox * scale];
                }
            }
            assert_eq!(
                one, two,
                "scale={scale} crop=({cx},{cy},{cw},{ch})：一趟与两趟结果不一致"
            );
        }
    }
}

// ---------- decode::planes_from_yuv420（Android 解码像素提取，跨平台纯函数）----------

use game_login_scanner_lib::decode::{planes_from_yuv420, ChromaLayout};

/// 半平面（NV12）：stride 大于宽时，Y 必须逐行取前 w 字节（跳过 padding）。
/// 这是 Android MediaCodec 输出最常见布局，也是扫码唯一依赖的像素路径。
#[test]
fn planes_semiplanar_y_skips_stride_padding() {
    let (w, h, stride) = (4usize, 2usize, 6usize); // 行跨距 6 > 宽 4
    let mut data = vec![0u8; stride * h + stride * (h / 2)];
    // Y 行 0 = 1,2,3,4 + padding 0xAA,0xAA；行 1 = 5,6,7,8 + padding
    data[0..4].copy_from_slice(&[1, 2, 3, 4]);
    data[4..6].copy_from_slice(&[0xAA, 0xAA]);
    data[6..10].copy_from_slice(&[5, 6, 7, 8]);
    data[10..12].copy_from_slice(&[0xAA, 0xAA]);
    // UV 行：紧接 Y 平面（y_plane = stride*slice_h = 12），交错 U,V
    data[12..18].copy_from_slice(&[10, 20, 11, 21, 0, 0]);
    let (y, u, v) = planes_from_yuv420(w, h, stride, h, ChromaLayout::SemiPlanar, &data);
    assert_eq!(
        y,
        vec![1, 2, 3, 4, 5, 6, 7, 8],
        "Y 必须紧凑且逐行跳 padding"
    );
    assert_eq!(u, vec![10, 11]);
    assert_eq!(v, vec![20, 21]);
}

/// 平面（I420）：U、V 各占连续区域。
#[test]
fn planes_planar_extracts_separate_u_v() {
    let (w, h, stride) = (4usize, 2usize, 4usize);
    // Y(8) + U(2) + V(2)，stride=4，ch=1，cstride=2
    let data = vec![
        1, 2, 3, 4, 5, 6, 7, 8, // Y
        30, 31, // U
        40, 41, // V
    ];
    let (y, u, v) = planes_from_yuv420(w, h, stride, h, ChromaLayout::Planar, &data);
    assert_eq!(y, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(u, vec![30, 31]);
    assert_eq!(v, vec![40, 41]);
}

/// 数据不足时不 panic，越界部分填 0（Y）/ 中性灰 128（U/V）。
#[test]
fn planes_short_buffer_is_safe() {
    // 只够第一行（4 字节）；后续行越界不拷贝，色度全越界。
    let data = vec![9u8; 4];
    let (y, u, v) = planes_from_yuv420(4, 4, 4, 4, ChromaLayout::SemiPlanar, &data);
    assert_eq!(y.len(), 16);
    assert!(y[0..4].iter().all(|&b| b == 9), "首行应完整拷贝");
    assert!(y[4..].iter().all(|&b| b == 0), "越界行应为 0");
    assert!(u.iter().all(|&b| b == 128), "越界 U 应为中性灰");
    assert!(v.iter().all(|&b| b == 128), "越界 V 应为中性灰");
}

// ---------- decode::sps_dimensions（Android 解码器 configure 所需宽高）----------

use game_login_scanner_lib::decode::sps_dimensions;

/// 测试用比特写入器（独立于被测解析器）。
struct BitWriter {
    bits: Vec<u8>,
}
impl BitWriter {
    fn new() -> Self {
        Self { bits: Vec::new() }
    }
    fn bit(&mut self, b: u32) {
        self.bits.push((b & 1) as u8);
    }
    fn bits(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.bit((v >> i) & 1);
        }
    }
    /// 无符号指数哥伦布 ue(v)。
    fn ue(&mut self, v: u32) {
        let v1 = v + 1;
        let n = 32 - v1.leading_zeros(); // 有效位宽
        for _ in 0..(n - 1) {
            self.bit(0);
        }
        self.bits(v1, n);
    }
    fn finish(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.bits.len().div_ceil(8)];
        for (i, &b) in self.bits.iter().enumerate() {
            if b == 1 {
                out[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        out
    }
}

/// 构造一个 baseline SPS（无裁剪），宽高按宏块换算。
fn make_sps(width_mb: u32, height_mb: u32) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.bits(66, 8); // profile_idc = baseline
    w.bits(0, 8); // constraint flags
    w.bits(30, 8); // level_idc
    w.ue(0); // seq_parameter_set_id
    w.ue(0); // log2_max_frame_num_minus4
    w.ue(0); // pic_order_cnt_type
    w.ue(0); // log2_max_pic_order_cnt_lsb_minus4
    w.ue(1); // max_num_ref_frames
    w.bit(0); // gaps_in_frame_num_value_allowed_flag
    w.ue(width_mb - 1); // pic_width_in_mbs_minus1
    w.ue(height_mb - 1); // pic_height_in_map_units_minus1
    w.bit(1); // frame_mbs_only_flag
    w.bit(1); // direct_8x8_inference_flag
    w.bit(0); // frame_cropping_flag
    let payload = w.finish();
    let mut out = vec![0u8, 0, 0, 1, 0x67]; // Annex-B + SPS NAL 头
    out.extend_from_slice(&payload);
    out
}

#[test]
fn sps_dimensions_parses_720p() {
    assert_eq!(sps_dimensions(&make_sps(80, 45)), Some((1280, 720)));
}

#[test]
fn sps_dimensions_parses_1080p_no_crop() {
    // 1088 = 68*16（真实 1080p 靠裁剪到 1080，这里验证无裁剪路径）
    assert_eq!(sps_dimensions(&make_sps(120, 68)), Some((1920, 1088)));
}

#[test]
fn sps_dimensions_none_without_sps() {
    let data = vec![0u8, 0, 0, 1, 0x68, 0xAB, 0, 0, 0, 1, 0x65, 0x88];
    assert_eq!(sps_dimensions(&data), None);
}

#[test]
fn sps_dimensions_finds_sps_after_other_nalus() {
    let mut data = vec![0u8, 0, 0, 1, 0x06, 0x05, 0x01]; // SEI
    data.extend_from_slice(&make_sps(80, 45));
    assert_eq!(sps_dimensions(&data), Some((1280, 720)));
}

// ---------- android 剪贴板载荷解析（跨平台纯函数） ----------

#[test]
fn clipboard_payload_parses_rgba() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    let (rgba, w, h) = qr::parse_clipboard_payload(&bytes).unwrap();
    assert_eq!((w, h), (2, 1));
    assert_eq!(rgba, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn clipboard_payload_rejects_malformed() {
    // 头部不足 8 字节
    assert!(qr::parse_clipboard_payload(&[0u8; 7]).is_none());
    // 头部齐但像素数据不足（2x2 需要 16 字节，只给 15）
    let mut short = Vec::new();
    short.extend_from_slice(&2u32.to_le_bytes());
    short.extend_from_slice(&2u32.to_le_bytes());
    short.extend_from_slice(&[0u8; 15]);
    assert!(qr::parse_clipboard_payload(&short).is_none());
    // 零尺寸
    let mut zero = Vec::new();
    zero.extend_from_slice(&0u32.to_le_bytes());
    zero.extend_from_slice(&1u32.to_le_bytes());
    assert!(qr::parse_clipboard_payload(&zero).is_none());
}

// ---------- 第三方渠道（协议层，2026-10-17 渠道扩展） ----------

#[test]
fn channel_table_matches_reference() {
    use game_login_scanner_lib::sdk::channel;
    // channel_id 映射与参考项目 bh3_login_simulation 一致
    assert_eq!(channel::parse("xiaomi").unwrap().id, 11);
    assert_eq!(channel::parse("yyb").unwrap().id, 13);
    assert_eq!(channel::parse("huawei").unwrap().id, 15);
    assert_eq!(channel::parse("vivo").unwrap().id, 19);
    assert_eq!(channel::parse("wegame").unwrap().id, 27);
    // 官服 / B站 不在渠道表（走各自已有链路）
    assert!(channel::parse("official").is_none());
    assert!(channel::parse("bilibili").is_none());
    assert!(channel::parse("unknown").is_none());
}

#[test]
fn verify_body_channel_shape_and_signature() {
    use game_login_scanner_lib::sdk::mihoyo;
    let (body, post) =
        mihoyo::build_verify_body("0000000000000000", 11, "{\"uid\":\"1\",\"session\":\"s\"}");
    assert_eq!(body["app_id"], 1);
    assert_eq!(body["channel_id"], 11);
    assert_eq!(body["data"], "{\"uid\":\"1\",\"session\":\"s\"}");
    // 签名 = HMAC-SHA256 hex，输入为排序拼接（app_id < channel_id < data < device）
    let sign = body["sign"].as_str().unwrap();
    assert_eq!(sign.len(), 64);
    let expect = mihoyo::bh3_sign(
        "app_id=1&channel_id=11&data={\"uid\":\"1\",\"session\":\"s\"}&device=0000000000000000",
    );
    assert_eq!(sign, expect);
    assert!(!post.contains(' '));
}

#[test]
fn parse_verify_response_handles_cases() {
    use game_login_scanner_lib::sdk::mihoyo;
    use serde_json::json;
    // 成功：提取 open_id / combo_id / combo_token
    let ok = json!({
        "retcode": 0,
        "data": { "open_id": "o1", "combo_id": "c1", "combo_token": "t1" }
    });
    let info = mihoyo::parse_verify_response(ok).unwrap();
    assert_eq!(info.open_id, "o1");
    assert_eq!(info.combo_id, "c1");
    assert_eq!(info.combo_token, "t1");
    // 业务错误：透传 message（参考项目 -107「参数错误」即走此路径）
    let err = json!({ "retcode": -107, "message": "参数错误" });
    assert!(mihoyo::parse_verify_response(err)
        .unwrap_err()
        .contains("参数错误"));
    // 缺 data
    let bad = json!({ "retcode": 0 });
    assert!(mihoyo::parse_verify_response(bad).is_err());
}

#[test]
fn channel_account_cred_encrypted_on_save_roundtrip() {
    // 渠道账号 cred 与 B站 / 官服令牌同等待遇：落盘密文、内存明文、读回一致。
    let mut cfg = AppConfig::default();
    cfg.channel_accounts
        .push(game_login_scanner_lib::config::ChannelAccount {
            channel: "xiaomi".into(),
            uid: "10001".into(),
            name: "小米账号".into(),
            cred: "{\"uid\":\"10001\",\"session\":\"MI-SESSION\"}".into(),
            ..Default::default()
        });
    let mut disk = cfg.clone();
    disk.encrypt_secrets();
    #[cfg(windows)]
    {
        assert_ne!(
            disk.channel_accounts[0].cred,
            "{\"uid\":\"10001\",\"session\":\"MI-SESSION\"}"
        );
    }
    disk.decrypt_secrets();
    assert_eq!(
        disk.channel_accounts[0].cred,
        "{\"uid\":\"10001\",\"session\":\"MI-SESSION\"}"
    );
    // 序列化往返（`#[serde(default)]` 保证旧配置无该字段时兼容）。
    let text = serde_json::to_string(&disk).unwrap();
    let back: AppConfig = serde_json::from_str(&text).unwrap();
    assert_eq!(back.channel_accounts.len(), 1);
    assert_eq!(back.channel_accounts[0].channel, "xiaomi");
    assert_eq!(back.channel_accounts[0].uid, "10001");
}
