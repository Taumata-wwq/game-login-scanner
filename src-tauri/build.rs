fn main() {
    tauri_build::build();

    // tauri v2 在 Windows 的已知问题：cargo test 的测试二进制不带应用 manifest
    // （tauri-winres 生成的资源只链接到 bin 目标），加载 comctl32 v6 入口点
    // （TaskDialogIndirect 等）时进程启动即报 STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139)。
    // 为测试目标单独嵌入同款 manifest，MSVC / GNU 工具链均由 embed-resource 处理。
    //
    // 注意：compile_for_tests 会发出 `cargo:rustc-link-arg-tests` 指令，而 cargo 在
    // 「包无 test target」时拒绝该指令（error: does not have a test target）。
    // tests/ 自 2026-10-17 起以白名单方式部分入库（unit.rs / smoke.rs 供 CI 质量门，
    // 探针/基准/批量测试仍为本地材料）——常规检出存在 tests/，但仍先确认其存在再调用：
    // 若目录缺失（如裁剪过的检出）则不发出测试指令，避免重演「无 test target」必败
    // （2026-10-15 定位的教训；与 rustc 1.98/1.99 版本无关）。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let tests_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        if tests_dir.is_dir() {
            let _ = embed_resource::compile_for_tests("tests-manifest.rc", embed_resource::NONE);
        }
    }
}
