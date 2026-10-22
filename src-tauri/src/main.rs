// GUI 应用：始终隐藏控制台窗口（避免启动时弹出终端）。
#![windows_subsystem = "windows"]

fn main() {
    // 单实例守卫：双开会让两份实例互相覆盖配置，且 WebView2 创建存在竞态
    // （实测：双开触发 tao「cannot move state from Destroyed」panic，
    // 并留下一个 webview 创建失败的僵尸进程）。用监听端口充当进程级互斥：
    // 绑定成功才继续；已有实例时静默退出。
    let _guard = match std::net::TcpListener::bind("127.0.0.1:47813") {
        Ok(l) => l,
        Err(_) => std::process::exit(0),
    };
    game_login_scanner_lib::run();
}
