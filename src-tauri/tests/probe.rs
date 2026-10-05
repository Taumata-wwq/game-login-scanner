//! 登录链路真实环境探针（手动运行，不进入常规测试套件）：
//! `cargo test --test probe -- --ignored --nocapture`
//!
//! 读取真实配置中的账号凭据，逐个调用 verify / OA 分发，
//! 验证各 API 步骤是否可用（不消费任何游戏二维码）。

use bh3_scanner_lib::config::AppConfig;
use bh3_scanner_lib::sdk::{bili, mihoyo, official};

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
    } else {
        println!("\n无官服账号（跳过）");
    }
}

/// 双渠道二维码同时生成探针（与 login_qr_start 双会话逻辑一致）。
#[test]
#[ignore = "真实网络探针，需手动运行"]
fn probe_dual_qr() {
    use bh3_scanner_lib::sdk::{bili, official};
    let rt = tokio::runtime::Runtime::new().expect("创建运行时失败");
    rt.block_on(async {
        let (b, o) = tokio::join!(
            bili::qr_login_start(),
            official::qr_login_start("00000000-0000-0000-0000-000000000000", "1145141919810")
        );
        match b {
            Ok(s) => println!("[OK] B 服二维码: {}…", &s.qr_url[..s.qr_url.len().min(60)]),
            Err(e) => println!("[FAIL] B 服二维码: {e}"),
        }
        match o {
            Ok(s) => println!("[OK] 官服二维码: {}…", &s.qr_url[..s.qr_url.len().min(60)]),
            Err(e) => println!("[FAIL] 官服二维码: {e}"),
        }
    });
}
