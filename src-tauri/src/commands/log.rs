/// 前端日志落盘：窗口操作失败等前端异常写入日志文件，便于远程排查。
#[tauri::command]
pub(crate) fn frontend_log(level: String, msg: String) {
    match level.as_str() {
        "error" => log::error!("[web] {msg}"),
        "warn" => log::warn!("[web] {msg}"),
        _ => log::info!("[web] {msg}"),
    }
}

// ---------- 系统通知（Android；桌面为空实现） ----------
//
// 手机切到游戏里扫码时应用界面不可见，通知栏是唯一的状态出口：
// - 常驻：切后台时显示「后台是否在扫码 / 正在扫哪些直播间」，回前台撤下；
// - 即时：扫码成功、直播间下播等一次性提醒（可划掉）。
// 由前端按设置开关（notify_persistent / notify_live_status）决定是否调用。
