fn main() {
    tauri_build::build();

    // tauri v2 在 Windows 的已知问题：cargo test 的测试二进制不带应用 manifest
    // （tauri-winres 生成的资源只链接到 bin 目标），加载 comctl32 v6 入口点
    // （TaskDialogIndirect 等）时进程启动即报 STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139)。
    // 为测试目标单独嵌入同款 manifest，MSVC / GNU 工具链均由 embed-resource 处理。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let _ = embed_resource::compile_for_tests("tests-manifest.rc", embed_resource::NONE);
    }
}
