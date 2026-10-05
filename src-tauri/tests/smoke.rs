//! 集成测试入口：验证库的公开纯函数行为（与单测互补，保证包内存在 test 目标，
//! 使 build.rs 的 `rustc-link-arg-tests`（测试 manifest 修复）对 cargo 合法）。

#[test]
fn lib_smoke() {
    // 公开 API 冒烟：验证 crate 可链接、可构造默认配置。
    // （配置序列化往返在 unit.rs 覆盖。）
    let cfg = bh3_scanner_lib::config::AppConfig::default();
    assert_eq!(cfg.interval_ms, 150);
}
