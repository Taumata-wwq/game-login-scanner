//! 登录链路真实环境探针（手动运行，不进入常规测试套件）：
//! `cargo test --test probe -- --ignored --nocapture`
//!
//! 读取真实配置中的账号凭据，逐个调用 verify / OA 分发，
//! 验证各 API 步骤是否可用（不消费任何游戏二维码）。

use game_login_scanner_lib::config::AppConfig;
use game_login_scanner_lib::sdk::{bili, mihoyo, official};

/// 二维码生成探针：复现「添加账号两个渠道都无法显示二维码」。
#[test]
#[ignore = "真实网络探针，需手动运行"]
fn probe_qr_start() {
    let rt = tokio::runtime::Runtime::new().expect("创建运行时失败");
    rt.block_on(async {
        match bili::qr_login_start().await {
            Ok(s) => println!("[OK] B 服二维码: {}…", &s.qr_url[..s.qr_url.len().min(70)]),
            Err(e) => println!("[FAIL] B 服二维码: {e}"),
        }
        match official::qr_login_start("0000000000000000-uuid-probe", "1145141919810").await {
            Ok(s) => println!("[OK] 官服二维码: {}…", &s.qr_url[..s.qr_url.len().min(70)]),
            Err(e) => println!("[FAIL] 官服二维码: {e}"),
        }
    });
}

#[test]
#[ignore = "真实网络探针，需手动运行"]
fn live_probe() {
    let cfg = AppConfig::load();
    let rt = tokio::runtime::Runtime::new().expect("创建运行时失败");

    println!("当前游戏: {:?}", cfg.current_game);

    if let Some(b) = cfg.active_bili("bh3") {
        println!("\n== B 服账号: {} ({}) ==", b.uname, b.uid);
        rt.block_on(async {
            match mihoyo::verify(&b.uid, &b.access_key).await {
                Ok(info) => {
                    println!(
                        "[OK] verify: open_id={} combo_id={}",
                        info.open_id, info.combo_id
                    );
                    match mihoyo::get_oa_server().await {
                        Ok(d) => println!("[OK] OA 分发获取成功 ({} 字符)", d.chars().count()),
                        Err(e) => println!("[FAIL] OA 分发: {e}"),
                    }
                }
                Err(e) => println!("[FAIL] verify: {e}"),
            }
            match bili::get_user_info(&b.uid, &b.access_key).await {
                Ok(name) => println!("[OK] user.info: uname={name}"),
                Err(e) => println!("[FAIL] user.info: {e}"),
            }
        });
    } else {
        println!("\n无 B 服账号（跳过）");
    }

    if let Some(o) = cfg.active_official("bh3") {
        println!("\n== 官服账号: uid {} ==", o.uid);
        println!(
            "mid={} stoken前20={}",
            o.mid,
            &o.stoken[..o.stoken.len().min(20)]
        );
        // 官服确认直接用 stoken（不再有 getGameToken 端点），此处仅校验凭据非空。
        if o.stoken.is_empty() || o.mid.is_empty() {
            println!("[FAIL] 官服凭据不完整（stoken/mid 为空）");
        } else {
            println!("[OK] 官服凭据完整（stoken/mid 均非空）");
        }
        rt.block_on(async {
            match official::get_uname(&o.uid, &o.stoken, &o.mid).await {
                Ok(name) => println!("[OK] get_uname: {name}"),
                Err(e) => println!("[FAIL] get_uname: {e}"),
            }
        });
    } else {
        println!("\n无官服账号（跳过）");
    }
}

/// GDI 截屏 + 剪贴板读图探针（手动运行）：
/// `cargo test --test probe probe_capture_clipboard -- --ignored --nocapture`
#[test]
#[ignore = "需要真实桌面与剪贴板，需手动运行"]
fn probe_capture_clipboard() {
    let rects = game_login_scanner_lib::screenshot::screen_rects();
    println!("显示器: {rects:?}");
    if let Some(&(x, y, w, h)) = rects.first() {
        match game_login_scanner_lib::screenshot::capture_region(x, y, w, h) {
            Some(f) => {
                let sampled: Vec<u32> = f
                    .rgb
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .step_by(97)
                    .take(2000)
                    .map(|p| (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3)
                    .collect();
                let avg = sampled.iter().sum::<u32>() / sampled.len().max(1) as u32;
                println!(
                    "[OK] 截屏 {}x{}，抽样亮度均值 {avg}（纯黑=0，纯白=255）",
                    f.w, f.h
                );
            }
            None => println!("[FAIL] GDI 截屏失败"),
        }
    } else {
        println!("[FAIL] 未枚举到显示器");
    }
    match game_login_scanner_lib::qr::read_clipboard_image() {
        Some((rgb, w, h)) => println!("[OK] 剪贴板图片 {w}x{h}，{} 字节 RGB", rgb.len()),
        None => println!("[--] 剪贴板无图片（可先用图片工具复制一张图再跑）"),
    }
}

/// 解码性能探针（手动运行）：2560×1440 大图全分辨率 + 降采样路径耗时。
#[test]
#[ignore = "性能探针，需手动运行"]
fn probe_decode_perf() {
    let text = "https://x/qr_code_in_game.html?ticket=PERF";
    let size = 2560usize;
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Low).unwrap();
    let n = qr.size() as usize;
    let px = 6usize;
    let q = px * n;
    let off = (size - q) / 2;
    let mut rgb = vec![255u8; size * size * 3];
    for my in 0..n {
        for mx in 0..n {
            if qr.get_module(mx as i32, my as i32) {
                for yy in 0..px {
                    for xx in 0..px {
                        let i = ((off + my * px + yy) * size + (off + mx * px + xx)) * 3;
                        rgb[i] = 0;
                        rgb[i + 1] = 0;
                        rgb[i + 2] = 0;
                    }
                }
            }
        }
    }
    let t0 = std::time::Instant::now();
    let hit = game_login_scanner_lib::qr::decode_rgb(&rgb, size as u32, size as u32);
    println!(
        "[{:?}] 2560×1440 解码: {:?}（首帧含降采样快路径）",
        t0.elapsed(),
        hit.is_some()
    );
    let t1 = std::time::Instant::now();
    let hit = game_login_scanner_lib::qr::decode_rgb(&rgb, size as u32, size as u32);
    println!(
        "[{:?}] 同图复解（含指纹重复路径）: {:?}",
        t1.elapsed(),
        hit.is_some()
    );
}

/// 直播捕获探针（手动运行）：连接真实直播间，验证流解析 + FLV 解复用 + H.264 解码。
/// 默认 B站房间 6（哔哩哔哩直播，通常常驻开播），可用环境变量 LIVE_ROOM 覆盖。
#[test]
#[ignore = "真实网络探针，需手动运行"]
fn probe_live() {
    use std::time::Duration;
    let room = std::env::var("LIVE_ROOM").unwrap_or_else(|_| "6".into());
    let rt = tokio::runtime::Runtime::new().expect("运行时");
    rt.block_on(async {
        let t0 = std::time::Instant::now();
        match game_login_scanner_lib::live::start(room.clone()).await {
            Ok((w, h)) => {
                println!(
                    "[OK] B站房间 {room} 已连接 {w}x{h}（耗时 {:?}）",
                    t0.elapsed()
                );
                let mut new_frames = 0u32;
                for _ in 0..10 {
                    if game_login_scanner_lib::live::take_scan_frame().is_some() {
                        new_frames += 1;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                println!("[OK] 2 秒内新帧数 {new_frames}（约 {}fps）", new_frames / 2);
                game_login_scanner_lib::live::stop();
            }
            Err(e) => println!("[FAIL] B站房间 {room}: {e}"),
        }
    });
}
