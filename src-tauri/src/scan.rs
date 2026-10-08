//! 扫码主流程编排：剪贴板优先，未命中则全屏高频截图找码。
//!
//! 「目标二维码」判定：解码结果能提取出登录票据即为目标码。米哈游系游戏码为
//! `qr_code_in_game.html?ticket=`（B 服与官服形态相同、平台无法从 URL 区分，
//! 确认阶段用当前渠道账号路由）；鹰角游戏码为 `hypergryph://scan_login?scanId=`
//! （用鹰角账号确认）。未登录任何账号时提示并忽略；`mobile.html?tk=` 为米游社
//! 通行证码，忽略。
//! 对同一张已失败的二维码做退避与日志去重，避免向服务器高频轰炸同一失效码。
//!
//! 性能策略：
//! - 解码用 zedbar（唯一解码器），整屏单次解码（真实 2K 截图 ~160ms）；
//! - 画面内容未变化的屏幕（抽样指纹一致）整帧跳过解码；
//! - 桌面复制「无新帧零成本跳过」。
//!
//! 稳态下 CPU 占用集中在变化的屏幕解码上，可维持高频检测。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::Emitter;
use tauri::Manager;

use crate::config::{BiliToken, Channel, Game, OfficialToken};
use crate::qr::{self, QrCode, QrKind};
use crate::sdk::{hypergryph, mihoyo, official};

/// 同一失败票据的重试退避间隔。
const FAILED_TICKET_BACKOFF: Duration = Duration::from_millis(1500);
/// 「用户请求停止」标记：confirm 经 select! 被取消时返回，上层据此终止扫描。
const STOP_MARKER: &str = "__stopped__";
/// 缩略图抽样步长（像素）。
const THUMB_STRIDE: u32 = 16;
/// 「非登录码」诊断提示的最大次数。
const UNKNOWN_QR_REPORTS: u32 = 3;

/// 扫描状态事件载荷。
#[derive(Debug, Clone, Serialize)]
struct ScanEvent {
    #[serde(rename = "type")]
    kind: &'static str,
    message: String,
}

fn emit(app: &tauri::AppHandle, kind: &'static str, message: impl Into<String>) {
    let _ = app.emit(
        "scan-status",
        ScanEvent {
            kind,
            message: message.into(),
        },
    );
}

/// 单次识别结果。
enum Handled {
    /// 尚无已登录的可用账号，跳过（每个票据只提示一次）。
    MissingAccount,
    /// 该码无法用于游戏登录确认（如米游社通行证码），忽略。
    Ignored,
    /// 提交失败，附带原因。
    Failed(String),
    /// 确认成功。
    Success,
}

/// 扫码运行上下文：**当前选中渠道**的账号凭据 + 按需缓存的运行时状态。
///
/// 游戏登录码是通用的（任何渠道账号都能确认登录），因此这里**只持有当前渠道**
/// 对应的账号，扫描确认时只用它——不做「逐个渠道尝试」。
struct ScanContext {
    /// 当前用于确认的渠道（来自配置 `current_channel`）。
    current: Channel,
    /// 当前游戏（决定 app_id 等游戏相关参数）。
    game: Game,
    /// 鹰角账号（终末地/明日方舟用）。
    hypergryph: hypergryph::HypergryphToken,
    bili: BiliToken,
    official: OfficialToken,
    device_id: String,
    /// B 服 bh_info（`verify_cached` 带进程级缓存，预校验预热后确认零等待）。
    bh_info: Option<mihoyo::BhInfo>,
    /// B 服账号被服务端明确拒绝（令牌过期等）：本会话内不再重试校验。
    bili_broken: bool,
    /// 停止信号（用于即时取消挂起的网络确认）。
    stop: Arc<AtomicBool>,
    /// 本会话已尝试确认过的票据（用于去重提示）。
    tried_tickets: std::collections::HashSet<String>,
    /// 游戏内显示的账号名（B 服 confirm 的 asterisk_name）。
    display_name: String,
}

impl ScanContext {
    fn current_ready(&self) -> bool {
        match self.current {
            Channel::Bilibili => !self.bili.uid.is_empty() && !self.bili.access_key.is_empty(),
            Channel::Official => !self.official.stoken.is_empty() && !self.official.mid.is_empty(),
        }
    }

    /// 按二维码类型处理。
    async fn confirm(&mut self, app: &tauri::AppHandle, qrc: &QrCode) -> Handled {
        match qrc.kind {
            QrKind::Passport => Handled::Ignored,
            QrKind::InGame => self.confirm_in_game(app, &qrc.ticket, &qrc.biz_key).await,
            QrKind::Hypergryph => self.confirm_hypergryph(app, &qrc.ticket).await,
        }
    }

    /// 鹰角游戏码（终末地/明日方舟）：用鹰角账号 token 确认。
    async fn confirm_hypergryph(&mut self, app: &tauri::AppHandle, scan_id: &str) -> Handled {
        if self.hypergryph.token.is_empty() || self.hypergryph.phone.is_empty() {
            return Handled::MissingAccount;
        }
        let first_attempt = self.tried_tickets.insert(scan_id.to_string());
        if first_attempt {
            emit(app, "info", "识别到鹰角游戏登录码，正在用鹰角账号确认…");
        }
        let token = self.hypergryph.token.clone();
        match hypergryph::confirm_scan(scan_id, &token).await {
            Ok(()) => Handled::Success,
            Err(e) => Handled::Failed(e),
        }
    }

    /// 游戏登录码：**用当前选中渠道的账号**确认。
    /// `biz_key` 来自二维码，决定 combo/panda 端点前缀。
    async fn confirm_in_game(
        &mut self,
        app: &tauri::AppHandle,
        ticket: &str,
        biz_key: &str,
    ) -> Handled {
        if !self.current_ready() {
            return Handled::MissingAccount;
        }
        let ch = self.current;

        // 同一票据只提示一次「正在确认」；重试时静默，避免日志刷屏。
        let first_attempt = self.tried_tickets.insert(ticket.to_string());

        match self
            .confirm_with(app, ch, ticket, biz_key, first_attempt)
            .await
        {
            Ok(()) => Handled::Success,
            Err(e) => Handled::Failed(e),
        }
    }

    /// 用指定渠道的账号确认游戏登录码。
    /// 与停止信号 `select!` 竞争：停止时立即丢弃挂起的网络请求（不再等超时），
    /// 返回 `STOP_MARKER` 供上层终止扫描。
    async fn confirm_with(
        &mut self,
        app: &tauri::AppHandle,
        ch: Channel,
        ticket: &str,
        biz_key: &str,
        verbose: bool,
    ) -> Result<(), String> {
        let stop = self.stop.clone();
        tokio::select! {
            r = self.confirm_inner(app, ch, ticket, biz_key, verbose) => r,
            _ = wait_for_stop(&stop) => Err(STOP_MARKER.to_string()),
        }
    }

    async fn confirm_inner(
        &mut self,
        app: &tauri::AppHandle,
        ch: Channel,
        ticket: &str,
        biz_key: &str,
        verbose: bool,
    ) -> Result<(), String> {
        match ch {
            Channel::Bilibili => {
                if self.bili_broken {
                    return Err(
                        "B 服账号校验未通过（令牌可能已过期），请重新登录该账号".to_string()
                    );
                }
                if self.bh_info.is_none() {
                    if verbose {
                        emit(app, "info", "正在校验 B 服账号…");
                    }
                    match mihoyo::verify_cached(&self.bili.uid, &self.bili.access_key).await {
                        Ok(info) => self.bh_info = Some(info),
                        Err(e) => {
                            // 服务端明确拒绝（令牌过期/无效）：本会话不再重试；
                            // 网络类错误（请求失败/读取失败）保持可重试。
                            if !(e.starts_with("米哈游请求失败") || e.starts_with("读取响应失败"))
                            {
                                self.bili_broken = true;
                                return Err(format!(
                                    "B 服账号校验失败: {e}（请在「账号」中重新登录该账号）"
                                ));
                            }
                            return Err(format!("B 服账号校验失败: {e}"));
                        }
                    }
                }
                let info = self.bh_info.as_ref().expect("bh_info 已就绪");
                if verbose {
                    emit(
                        app,
                        "info",
                        format!("识别到{}登录码，正在用 B 服账号确认…", self.game.label()),
                    );
                }
                mihoyo::scan_confirm(
                    info,
                    ticket,
                    biz_key,
                    &self.display_name,
                    self.game.app_id(),
                )
                .await
            }
            Channel::Official => {
                if verbose {
                    emit(
                        app,
                        "info",
                        format!("识别到{}登录码，正在用官服账号确认…", self.game.label()),
                    );
                }
                official::scan(
                    ticket,
                    &self.official.stoken,
                    &self.official.mid,
                    &self.device_id,
                    biz_key,
                    self.game.app_id(),
                )
                .await
            }
        }
    }
}

/// 轮询停止信号（供 select! 竞争）。
async fn wait_for_stop(stop: &Arc<AtomicBool>) {
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

/// 扫描循环内的去重/退避/记忆状态。
struct LoopState {
    last_failed_ticket: Option<String>,
    backoff_until: tokio::time::Instant,
    reported_tickets: HashSet<String>,
    screens_empty_reported: bool,
    /// 各显示器上一帧的抽样指纹（用于跳过未变化的屏幕）。
    last_thumbs: Vec<Option<Vec<u8>>>,
    unknown_qr_reported: u32,
    /// 每个票据的失败次数（超过上限后跳过，避免无限重试同一失效码）。
    failed_count: std::collections::HashMap<String, u32>,
    /// 触发成功频率限制：终止扫描（服务端已确认成功，继续扫只会重复提交）。
    throttle_stop: bool,
    /// 用户请求停止且已取消挂起操作：立即终止扫描。
    stopped: bool,
}

/// 同一票据确认失败的上限：达到后不再尝试该票据。
const MAX_FAILS_PER_TICKET: u32 = 3;
/// failed_count 软上限：超过后丢弃最旧记录（去重提示为弱语义，丢弃不影响正确性）。
const FAILED_COUNT_MAX: usize = 256;
/// reported_tickets 软上限：超过后整批清空。
const REPORTED_TICKETS_MAX: usize = 1024;

impl LoopState {
    fn new() -> Self {
        Self {
            last_failed_ticket: None,
            backoff_until: tokio::time::Instant::now(),
            reported_tickets: HashSet::new(),
            screens_empty_reported: false,
            last_thumbs: Vec::new(),
            unknown_qr_reported: 0,
            failed_count: std::collections::HashMap::new(),
            throttle_stop: false,
            stopped: false,
        }
    }

    /// 修剪累积的去重状态，保持内存有界。超过上限时整批清空（比逐条淘汰简单，
    /// 且不会误伤最新票据的退避逻辑——退避由 last_failed_ticket 单独维护）。
    fn trim(&mut self) {
        if self.failed_count.len() > FAILED_COUNT_MAX {
            let keep = self.last_failed_ticket.clone();
            self.failed_count.clear();
            if let Some(t) = keep {
                self.failed_count.insert(t, 1);
            }
        }
        if self.reported_tickets.len() > REPORTED_TICKETS_MAX {
            self.reported_tickets.clear();
        }
    }
}

/// 抽样生成画面灰度指纹；两帧指纹相同则认为画面未变化。
fn thumbnail(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let iw = width as usize;
    let mut out = Vec::new();
    let mut y = 0u32;
    while y < height {
        let mut x = 0u32;
        while x < width {
            let i = (y as usize * iw + x as usize) * 3;
            if i + 2 < rgb.len() {
                out.push(((rgb[i] as u16 + rgb[i + 1] as u16 + rgb[i + 2] as u16) / 3) as u8);
            }
            x += THUMB_STRIDE;
        }
        y += THUMB_STRIDE;
    }
    out
}

/// 把服务端错误码/文案映射为带操作建议的提示。
fn friendly_error(raw: &str) -> String {
    // 常见错误码（来自米哈游/B 站服务端文档与实测）。
    if raw.contains("-1001") || raw.contains("操作过于频繁") {
        return format!("{raw}（服务端限流，请稍等 1~2 分钟再试）");
    }
    if raw.contains("-107") || raw.contains("渠道错误") {
        return format!("{raw}（渠道参数异常，请重新登录该账号）");
    }
    if raw.contains("-5300") || raw.contains("请使用最新版本产品") {
        return format!("{raw}（该接口已废弃，请改用其他方式登录）");
    }
    if raw.contains("decode err") {
        // stoken 失效/过期时 getGameToken 常返回该错误。
        return format!("{raw}（账号令牌可能已过期，请在「账号」中重新登录该账号）");
    }
    if raw.contains("expire") || raw.contains("过期") {
        return format!("{raw}（二维码已过期，请在游戏内刷新登录界面）");
    }
    raw.to_string()
}

/// 扫码命中后的收尾（三条命中路径共用）。
/// 触发成功频率限制时：服务端已确认、游戏已登录，但不再执行「成功后动作」，
/// 提示后直接停止扫描；正常路径执行清剪贴板 / 自动退出。两条路径都结束扫描。
async fn finish_success(app: &tauri::AppHandle, st: &LoopState, auto_close: bool) {
    if st.throttle_stop {
        emit(
            app,
            "stopped",
            "已达扫码成功频率上限（2 分钟 10 次），已停止扫描",
        );
        return;
    }
    emit(app, "success", "扫码成功");
    finalize_success(app, auto_close).await;
}

/// 处理一个候选登录码：确认成功返回 true；失败/未登录/忽略按去重与退避策略记录。
async fn try_confirm(
    app: &tauri::AppHandle,
    ctx: &mut ScanContext,
    qrc: &QrCode,
    st: &mut LoopState,
) -> bool {
    st.trim();
    // 会话内已尝试票据集合的软上限（长时间挂机防膨胀）。
    if ctx.tried_tickets.len() > REPORTED_TICKETS_MAX {
        ctx.tried_tickets.clear();
    }
    // 同一票据失败达上限：跳过并提示一次，等新二维码。
    if st.failed_count.get(&qrc.ticket).copied().unwrap_or(0) >= MAX_FAILS_PER_TICKET {
        if st
            .reported_tickets
            .insert("skip:".to_string() + &qrc.ticket)
        {
            emit(
                app,
                "error",
                "该二维码多次确认失败（可能已失效或被限流），请刷新游戏登录界面或稍后再试",
            );
        }
        return false;
    }

    let same_as_failed = st.last_failed_ticket.as_deref() == Some(&qrc.ticket);
    if same_as_failed && tokio::time::Instant::now() < st.backoff_until {
        return false;
    }
    match ctx.confirm(app, qrc).await {
        Handled::Success => {
            // 成功频率限制：2 分钟内最多 10 次成功。只对「成功」记账，
            // 失败 / 重试不占用配额（弱网下避免烧光成功额度）。
            if let Err(msg) = crate::check_scan_throttle(app) {
                if st.reported_tickets.insert("__throttle__".to_string()) {
                    emit(app, "error", msg);
                }
                // 服务端已确认成功（游戏已登录）：终止扫描防止对已消费票据重复提交，
                // 但不执行「成功后动作」（清剪贴板 / 自动退出）。
                st.throttle_stop = true;
                return true;
            }
            true
        }
        Handled::MissingAccount => {
            if st.reported_tickets.insert(qrc.ticket.clone()) {
                emit(
                    app,
                    "warn",
                    format!(
                        "识别到{}登录码，但尚未登录任何渠道账号，请先在「账号」中登录",
                        ctx.game.label()
                    ),
                );
            }
            false
        }
        Handled::Ignored => {
            if st.reported_tickets.insert(qrc.ticket.clone()) {
                emit(
                    app,
                    "warn",
                    "该二维码是米游社通行证码，无法用于游戏登录确认，已忽略",
                );
            }
            false
        }
        Handled::Failed(e) => {
            if e == STOP_MARKER {
                st.stopped = true;
                return false;
            }
            *st.failed_count.entry(qrc.ticket.clone()).or_insert(0) += 1;
            // 同一票据只报一次错，换新码才再次提示。
            if !same_as_failed {
                emit(app, "error", format!("扫码失败: {}", friendly_error(&e)));
                log::debug!("扫码失败 ticket={}: {e}", qrc.ticket);
            }
            st.last_failed_ticket = Some(qrc.ticket.clone());
            st.backoff_until = tokio::time::Instant::now() + FAILED_TICKET_BACKOFF;
            false
        }
    }
}

/// 扫码主流程入参（打包传入，避免参数过多）。
pub struct ScanArgs {
    pub current_channel: Channel,
    pub current_game: Game,
    pub hypergryph: hypergryph::HypergryphToken,
    pub bili: BiliToken,
    pub official: OfficialToken,
    pub device_id: String,
    pub auto_close: bool,
    pub stop: Arc<AtomicBool>,
    /// 游戏内显示的账号名（B 服 confirm 用）。
    pub display_name: String,
}

/// 扫码主流程。命中目标二维码并完成确认后返回；被停止返回「已停止」。
pub async fn run_scan(app: tauri::AppHandle, args: ScanArgs) {
    let ScanArgs {
        current_channel,
        current_game,
        hypergryph,
        bili,
        official,
        device_id,
        auto_close,
        stop,
        display_name,
    } = args;
    // 扫码循环固定 60Hz：扫码期间无论画面是否静止，检测频率不低于 60fps。
    // 静止屏幕由桌面复制「无新帧零成本跳过」+ 指纹去重，60Hz 轮询近零开销。
    let interval = Duration::from_millis(16);
    let ch_desc = if current_game.is_hypergryph() {
        "鹰角".to_string()
    } else {
        current_channel.label().to_string()
    };
    emit(
        &app,
        "started",
        format!("已开始扫描（使用{ch_desc}账号确认）"),
    );

    let mut ctx = ScanContext {
        current: current_channel,
        game: current_game,
        hypergryph,
        bili,
        official,
        device_id,
        bh_info: None,
        bili_broken: false,
        stop: stop.clone(),
        tried_tickets: std::collections::HashSet::new(),
        display_name,
    };
    let mut st = LoopState::new();

    // 1) 剪贴板优先（限时 500ms：剪贴板被其它进程长时间占用时直接跳过，
    //    否则表现为「点了开始扫码很久没反应」）。
    emit(&app, "info", "读取剪贴板…");
    let clipboard = {
        let read = tokio::task::spawn_blocking(qr::read_clipboard_image);
        match tokio::time::timeout(Duration::from_millis(500), read).await {
            Ok(Ok(v)) => v,
            _ => {
                emit(&app, "info", "剪贴板读取超时，跳过（直接全屏搜索）");
                None
            }
        }
    };
    if let Some((rgb, w, h)) = clipboard {
        if let Some(text) = qr::decode_rgb(&rgb, w, h) {
            if let Some(qrc) = qr::extract(&text) {
                if try_confirm(&app, &mut ctx, &qrc, &mut st).await {
                    finish_success(&app, &st, auto_close).await;
                    return;
                }
                if st.stopped {
                    emit(&app, "stopped", "已停止扫描");
                    return;
                }
            }
        }
    }

    emit(&app, "info", "全屏搜索登录二维码中…");

    // 2) 全屏/指定区域检测：
    //    桌面复制「无新帧即零成本跳过」→ 指纹去重 → zedbar 整屏解码。
    //    直播会话激活时改为扫直播帧（断流自动回退屏幕扫描）。
    let mut live_seen = false;
    loop {
        if stop.load(Ordering::SeqCst) {
            emit(&app, "stopped", "已停止扫描");
            return;
        }

        // 直播源优先：取自上次消费以来的新帧（seq 去重，静止/未变帧近零成本）。
        if crate::live::active() {
            if let Some((rgb, w, h)) = crate::live::take_scan_frame() {
                live_seen = true;
                // zedbar 解码（唯一解码器，真实截图实测最快最准）。
                let decoded = qr::decode_rgb(&rgb, w, h);
                if let Some(text) = decoded {
                    if let Some(qrc) = qr::extract(&text) {
                        if try_confirm(&app, &mut ctx, &qrc, &mut st).await {
                            if st.stopped {
                                emit(&app, "stopped", "已停止扫描");
                            } else {
                                finish_success(&app, &st, auto_close).await;
                            }
                            return;
                        }
                        if st.stopped {
                            emit(&app, "stopped", "已停止扫描");
                            return;
                        }
                    }
                }
            }
        } else {
            if live_seen {
                live_seen = false;
                emit(&app, "info", "直播已断开，回退屏幕扫描");
            }

            // 用户指定了扫描区域（会话内有效，不持久化）时只扫该区域。
            // Option<(i32,i32,i32,i32)> 实现了 Copy，直接解引用拷贝即可。
            let region = *app
                .state::<crate::AppState>()
                .region
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match region {
                Some(rect) => {
                    if process_screen(&app, &mut ctx, &mut st, 0, rect, auto_close).await {
                        if st.stopped {
                            emit(&app, "stopped", "已停止扫描");
                        } else {
                            finish_success(&app, &st, auto_close).await;
                        }
                        return;
                    }
                }
                None => {
                    let rects = crate::screenshot::screen_rects();
                    if rects.is_empty() {
                        if !st.screens_empty_reported {
                            emit(&app, "error", "屏幕捕获失败，请检查显示设置");
                            st.screens_empty_reported = true;
                        }
                    } else {
                        st.screens_empty_reported = false;
                    }
                    for (idx, rect) in rects.iter().enumerate() {
                        if stop.load(Ordering::SeqCst) {
                            emit(&app, "stopped", "已停止扫描");
                            return;
                        }
                        if process_screen(&app, &mut ctx, &mut st, idx, *rect, auto_close).await {
                            if st.stopped {
                                emit(&app, "stopped", "已停止扫描");
                            } else {
                                finish_success(&app, &st, auto_close).await;
                            }
                            return;
                        }
                    }
                }
            }
        }

        let now = tokio::time::Instant::now();
        let wait = if now < st.backoff_until {
            (st.backoff_until - now).max(interval)
        } else {
            interval
        };
        tokio::time::sleep(wait).await;
    }
}

/// 处理一块屏幕/区域：取帧（桌面复制优先，画面无新帧直接跳过；不可用走 GDI）
/// → 指纹去重 → zedbar 整屏解码。返回 true 表示扫描应结束。
async fn process_screen(
    app: &tauri::AppHandle,
    ctx: &mut ScanContext,
    st: &mut LoopState,
    idx: usize,
    rect: (i32, i32, i32, i32),
    auto_close: bool,
) -> bool {
    // 0) 取帧：桌面复制只在画面变化时给出帧，静止屏幕在此近零成本跳过。
    let frame = match crate::duplication::scan_frame(rect) {
        Ok(Some(f)) => f,
        Ok(None) => return false,
        Err(_) => match crate::screenshot::capture_region(rect.0, rect.1, rect.2, rect.3) {
            Some(f) => f,
            None => {
                if !st.screens_empty_reported {
                    emit(app, "error", "屏幕捕获失败，请检查显示设置");
                    st.screens_empty_reported = true;
                }
                return false;
            }
        },
    };
    st.screens_empty_reported = false;

    // 1) 指纹去重：微小变化（如光标闪烁）不触发解码。
    let thumb = thumbnail(&frame.rgb, frame.w, frame.h);
    if st.last_thumbs.get(idx).and_then(|t| t.as_ref()) == Some(&thumb) {
        return false;
    }
    if st.last_thumbs.len() <= idx {
        st.last_thumbs.resize(idx + 1, None);
    }
    st.last_thumbs[idx] = Some(thumb);

    // 2) zedbar 解码（唯一解码器）：真实 2560x1440 游戏登录截图实测全屏 ~157ms
    //    且 100% 命中。
    let Some(text) = qr::decode_rgb(&frame.rgb, frame.w, frame.h) else {
        return false;
    };
    let Some(qrc) = qr::extract(&text) else {
        // 诊断：能解码但不是登录码，提示原文片段便于排查（限次数）。
        if st.unknown_qr_reported < UNKNOWN_QR_REPORTS {
            st.unknown_qr_reported += 1;
            let preview: String = text.chars().take(60).collect();
            emit(
                app,
                "info",
                format!("发现二维码但不是{}登录码：{preview}…", ctx.game.label()),
            );
        }
        return false;
    };
    if try_confirm(app, ctx, &qrc, st).await {
        finish_success(app, st, auto_close).await;
        return true;
    }
    st.stopped
}

/// 成功后续动作：按配置自动退出。
/// 退出前统一走 `crate::save_window_position`，避免自动退出时丢失窗口位置
/// （`CloseRequested` 事件不会在 `app.exit()` 路径上触发）。
async fn finalize_success(app: &tauri::AppHandle, auto_close: bool) {
    if auto_close {
        tokio::time::sleep(Duration::from_millis(500)).await;
        log::info!("扫码成功，按配置自动退出");
        crate::save_window_position(app);
        app.exit(0);
    }
}
