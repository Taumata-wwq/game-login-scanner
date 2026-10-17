//! 真实直播帧上的二维码解码基准：对比「主扫描 / 生产有界重试 / 多线程分块并行」。
//!
//! 素材：`.workbuddy/tmp/live_frames/*.gray`（1 字节/像素）+ meta.txt（宽 高）。
//! 用法：`cargo test --profile fastbench --test bench_qr_live -- --ignored --nocapture`
//!
//! 两个维度：
//! 1) 耗时（无码画面，最常见）：主扫描 only / 生产路径 / 分块并行（2/4/8 线程）
//! 2) 检出能力（贴入合成二维码，含跨分块边界位置）：各策略是否检出 + 耗时

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

fn frames_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".workbuddy/tmp/live_frames")
}

fn load_frames() -> Vec<(Vec<u8>, u32, u32)> {
    let dir = frames_dir();
    let meta = std::fs::read_to_string(dir.join("meta.txt"))
        .expect("缺少 meta.txt（先跑 probe_capture_frames）");
    let mut out = Vec::new();
    for line in meta.lines() {
        let mut it = line.split_whitespace();
        let (Some(name), Some(w), Some(h)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) else {
            continue;
        };
        if let Ok(data) = std::fs::read(dir.join(name)) {
            out.push((data, w, h));
        }
    }
    out
}

fn zedbar_scan(gray: &[u8], w: u32, height: u32, density: u32, retry: bool) -> usize {
    let Ok(mut img) = zedbar::Image::from_gray(gray, w, height) else {
        return 0;
    };
    let mut cfg = zedbar::DecoderConfig::new()
        .enable(zedbar::config::QrCode)
        .position_tracking(true)
        .retry_undecoded_regions(retry);
    if density > 1 {
        cfg = cfg.scan_density(density, density);
    }
    let mut sc = zedbar::Scanner::with_config(cfg);
    sc.scan(&mut img).symbols().len()
}

/// 分块并行解码原型：横向条带（含重叠），各线程独立扫描，任一命中即返回。
fn decode_tiled(gray: &[u8], w: u32, height: u32, threads: usize, overlap: u32) -> Option<String> {
    let threads = threads.max(1);
    if threads == 1 {
        return decode_strip(gray, w, height);
    }
    let rows_per = height.div_ceil(threads as u32);
    let (tx, rx) = mpsc::channel::<String>();
    let mut handles = Vec::new();
    for i in 0..threads {
        let y0 = (i as u32 * rows_per).min(height);
        if y0 >= height {
            break;
        }
        let y1 = ((i as u32 + 1) * rows_per + overlap).min(height);
        let strip = gray[(y0 * w) as usize..(y1 * w) as usize].to_vec();
        let (w, hh) = (w, y1 - y0);
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            if let Some(text) = decode_strip(&strip, w, hh) {
                let _ = tx.send(text);
            }
        }));
    }
    drop(tx);
    let got = rx.recv().ok();
    for h in handles {
        let _ = h.join();
    }
    got
}

fn decode_strip(gray: &[u8], w: u32, height: u32) -> Option<String> {
    let mut img = zedbar::Image::from_gray(gray, w, height).ok()?;
    let cfg = zedbar::DecoderConfig::new()
        .enable(zedbar::config::QrCode)
        .scan_density(2, 2);
    let mut sc = zedbar::Scanner::with_config(cfg);
    let res = sc.scan(&mut img);
    res.symbols()
        .first()
        .and_then(|s| s.data_string().map(|t| t.to_string()))
}

// ---------- 第三方解码器对照（zxing-cpp / rxing） ----------

/// zxing-cpp（C++ 内核，QR only，C++ 默认选项：含 tryRotate/tryDownscale）。
fn zxingcpp_decode(gray: &[u8], w: u32, h: u32) -> usize {
    let Ok(iv) =
        zxingcpp::ImageView::from_slice(gray, w as i32, h as i32, zxingcpp::ImageFormat::Lum)
    else {
        return 0;
    };
    let reader = zxingcpp::read().formats([zxingcpp::BarcodeFormat::QRCode]);
    match reader.from(iv) {
        Ok(v) => v.len(),
        Err(_) => 0,
    }
}

/// rxing（纯 Rust ZXing 移植，QR only，默认 hints）。
fn rxing_decode(gray: &[u8], w: u32, h: u32) -> usize {
    use rxing::Reader as _;
    let Ok(src) = rxing::Luma8Source::new_with_slice(gray, w, h) else {
        return 0;
    };
    let mut bitmap = rxing::BinaryBitmap::new(rxing::common::HybridBinarizer::new(src));
    let hints = rxing::DecodeHints::default().with(rxing::DecodeHintValue::PossibleFormats(
        std::collections::HashSet::from([rxing::BarcodeFormat::QR_CODE]),
    ));
    let mut reader = rxing::MultiFormatReader::default();
    match reader.decode_with_hints(&mut bitmap, &hints) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// 在灰度帧中贴入一个合成二维码（**带白色静区**，贴近真实游戏登录码形态）。
fn paste_qr(gray: &mut [u8], w: u32, h: u32, qr_size: u32, x0: u32, y0: u32) -> String {
    let text = "https://x/qr_code_in_game.html?ticket=LIVEBENCH";
    let code = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Low).unwrap();
    let n = code.size() as u32;
    let px = (qr_size / n).max(1);
    let q = px * n;
    let margin = (q / 12).max(8); // 静区（约 4 模块宽）
                                  // 白色底板（静区）
    for y in y0.saturating_sub(margin)..(y0 + q + margin).min(h) {
        for x in x0.saturating_sub(margin)..(x0 + q + margin).min(w) {
            gray[(y * w + x) as usize] = 255;
        }
    }
    for my in 0..n {
        for mx in 0..n {
            let dark = code.get_module(mx as i32, my as i32);
            let v = if dark { 0u8 } else { 255u8 };
            for yy in 0..px {
                for xx in 0..px {
                    let (px_x, px_y) = (x0 + mx * px + xx, y0 + my * px + yy);
                    if px_x < w && px_y < h {
                        gray[(px_y * w + px_x) as usize] = v;
                    }
                }
            }
        }
    }
    text.to_string()
}

/// 检出能力矩阵：各解码器 × 位置/尺寸（贴入带静区的合成码）。
/// 解码器条目：(名称, 单帧解码函数)。
type EngineFn = fn(&[u8], u32, u32) -> bool;

fn detection_matrix(frames: &[(Vec<u8>, u32, u32)]) {
    let (fw, fh) = (frames[0].1, frames[0].2);
    let engines: [(&str, EngineFn); 4] = [
        ("zedbar生产", |g, w, h| {
            game_login_scanner_lib::qr::decode_gray_hit_fast(g, w, h).is_some()
        }),
        ("zedbar保真", |g, w, h| {
            game_login_scanner_lib::qr::decode_gray_hit(g, w, h).is_some()
        }),
        ("rxing", |g, w, h| rxing_decode(g, w, h) > 0),
        ("zxing-cpp", |g, w, h| zxingcpp_decode(g, w, h) > 0),
    ];
    let cases: [(u32, u32, u32, &str); 6] = [
        (180, 400, 200, "180px 中心"),
        (180, 200, 200, "180px 左侧"),
        (180, 900, 220, "180px 右侧"),
        (300, 480, 240, "300px 中心"),
        (120, 500, 300, "120px 小码"),
        (90, 500, 300, "90px 极小码"),
    ];
    println!("\n【检出能力矩阵（带静区合成码；样本帧 {fw}x{fh}）】");
    for (qr_size, x0, y0, label) in cases {
        let mut g = frames[0].0.clone();
        let _ = paste_qr(&mut g, fw, fh, qr_size, x0, y0);
        let mut line = format!("  {label:<14}");
        for (name, f) in engines.iter() {
            line.push_str(&format!(
                "| {name}={} ",
                if f(&g, fw, fh) { "命中" } else { "未命中" }
            ));
        }
        println!("{line}");
    }
}

#[test]
#[ignore = "基准测试，需手动运行"]
fn bench_qr_on_live_frames() {
    let frames = load_frames();
    assert!(!frames.is_empty(), "未找到样本帧");
    let (fw, fh) = (frames[0].1, frames[0].2);
    println!("样本：{} 帧 {fw}x{fh}", frames.len());

    // ---------- 1) 无码耗时 ----------
    let t = |label: &str, f: &dyn Fn(&[u8], u32, u32) -> usize| {
        let mut total = 0f64;
        for (g, w, h) in frames.iter().take(12) {
            let t0 = Instant::now();
            let _ = f(g, *w, *h);
            total += t0.elapsed().as_secs_f64() * 1000.0;
        }
        println!("  {label:<34} {:.2}ms/帧", total / 12.0);
    };
    println!("\n【无码画面（最常见）单帧耗时】");
    t("主扫描 density=2（无重试）", &|g, w, h| {
        zedbar_scan(g, w, h, 2, false)
    });
    t("保真 density=1（无重试）", &|g, w, h| {
        zedbar_scan(g, w, h, 1, false)
    });
    t("内置无界重试（旧生产）", &|g, w, h| {
        zedbar_scan(g, w, h, 2, true)
    });
    t("官方生产路径 decode_gray_hit_fast", &|g, w, h| {
        game_login_scanner_lib::qr::decode_gray_hit_fast(g, w, h).is_some() as usize
    });
    t("zxing-cpp（QR only，C++ 默认）", &zxingcpp_decode);
    t("rxing（QR only，快档，生产）", &rxing_decode);
    t("rxing（QR only + TRY_HARDER 兜底档）", &rxing_decode_harder);
    for th in [2usize, 4, 8] {
        t(
            &format!("分块并行 {th} 线程（重叠 0 行）"),
            &|g, w, h| decode_tiled(g, w, h, th, 0).is_some() as usize,
        );
    }
    for th in [4usize] {
        for ov in [64u32, 160, 320] {
            t(
                &format!("分块并行 {th} 线程（重叠 {ov} 行）"),
                &|g, w, h| decode_tiled(g, w, h, th, ov).is_some() as usize,
            );
        }
    }

    // ---------- 2) 检出能力矩阵 ----------
    detection_matrix(&frames);
}

/// 小码回归对比：沿用单测 `decode_small_qr_in_large_canvas` 的合成场景
/// （1000×1000 画布，70/100px 小码，白底黑码带静区），比较各引擎的**检出**能力。
#[test]
#[ignore = "基准测试，需手动运行"]
fn bench_small_qr_regression() {
    fn render_qr_gray(text: &str, size: usize) -> Vec<u8> {
        let code = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Low).unwrap();
        let n = code.size() as usize;
        let px = (size / n).max(1);
        let q = px * n;
        let off = (size - q) / 2;
        let mut g = vec![255u8; size * size];
        for my in 0..n {
            for mx in 0..n {
                if !code.get_module(mx as i32, my as i32) {
                    continue;
                }
                for yy in 0..px {
                    for xx in 0..px {
                        g[(off + my * px + yy) * size + off + mx * px + xx] = 0;
                    }
                }
            }
        }
        g
    }
    let text = "https://x/qr_code_in_game.html?ticket=SMALLQR";
    println!("\n【小码回归（1000×1000 画布，白底黑码）】");
    for qr_size in [70usize, 100, 140, 200] {
        let qr = render_qr_gray(text, qr_size);
        let size = 1000usize;
        let (off_x, off_y) = (450usize, 460usize);
        let mut g = vec![255u8; size * size];
        for y in 0..qr_size {
            for x in 0..qr_size {
                g[(y + off_y) * size + x + off_x] = qr[y * qr_size + x];
            }
        }
        let zed_prod =
            game_login_scanner_lib::qr::decode_gray_hit_fast(&g, size as u32, size as u32)
                .is_some();
        let zed_exact =
            game_login_scanner_lib::qr::decode_gray_hit(&g, size as u32, size as u32).is_some();
        let rx_plain = rxing_decode(&g, size as u32, size as u32) > 0;
        let rx_hard = rxing_decode_harder(&g, size as u32, size as u32) > 0;
        let zxcpp = zxingcpp_decode(&g, size as u32, size as u32) > 0;
        println!(
            "  {qr_size}px 码：zedbar生产={} zedbar保真={} rxing={} rxing+TRY_HARDER={} zxing-cpp={}",
            yn(zed_prod), yn(zed_exact), yn(rx_plain), yn(rx_hard), yn(zxcpp)
        );
    }
}

fn yn(b: bool) -> &'static str {
    if b {
        "命中"
    } else {
        "未命中"
    }
}

/// rxing + TRY_HARDER（更慢但更全，用于周期性精确兜底档）。
fn rxing_decode_harder(gray: &[u8], w: u32, h: u32) -> usize {
    use rxing::Reader as _;
    let Ok(src) = rxing::Luma8Source::new_with_slice(gray, w, h) else {
        return 0;
    };
    let mut bitmap = rxing::BinaryBitmap::new(rxing::common::HybridBinarizer::new(src));
    let hints = rxing::DecodeHints::default()
        .with(rxing::DecodeHintValue::PossibleFormats(
            std::collections::HashSet::from([rxing::BarcodeFormat::QR_CODE]),
        ))
        .with(rxing::DecodeHintValue::TryHarder(true));
    let mut reader = rxing::MultiFormatReader::default();
    match reader.decode_with_hints(&mut bitmap, &hints) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}
