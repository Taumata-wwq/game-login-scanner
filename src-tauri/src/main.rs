// GUI 应用：始终隐藏控制台窗口（避免启动时弹出终端）。
#![windows_subsystem = "windows"]

fn main() {
    bh3_scanner_lib::run();
}
