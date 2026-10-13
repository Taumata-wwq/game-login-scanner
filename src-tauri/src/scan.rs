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
use std::sync::{Arc, PoisonError};
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

/// 直播扫码的取帧流水线：把「取最新帧 + YUV→灰度转换」放到独立线程，
/// 与解码循环并行（同一 CPU 预算下检测频率 ≈ 提升到 max(转换, 解码) 的倒数）。
///
/// 背景（2026-10-13 基准实测，720p 真实直播帧）：主扫描 20.7ms/帧，灰度转换
/// （`take_scan_frame` 内的 YUV→灰度）约 12ms/帧；两者原本在扫码线程上**串行**
/// （合计 ≈33ms/轮）→ 检测频率被两者之和限制。流水线后一轮 ≈ max(20.7, 12) ≈ 21ms，
/// 相同 CPU 占用下检测频率提升约 40%（扫码等待"码出现"的时延同步缩短）。
///
/// 只做 1 帧深度的手递手：槽位非空时不转换（避免转换出消费者来不及解码的帧白白烧 CPU）。
struct LivePipe {
    slot: Arc<std::sync::Mutex<Option<crate::live::ScanFrame>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl LivePipe {
    /// 启动生产者线程（随 `stop` 停止；Drop 时 join）。
    fn start(stop: Arc<AtomicBool>) -> Self {
        let slot: Arc<std::sync::Mutex<Option<crate::live::ScanFrame>>> =
            Arc::new(std::sync::Mutex::new(None));
        let slot_prod = Arc::clone(&slot);
        let stop_prod = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("game-scanner-scan-pipe".into())
            .spawn(move || {
                let mut last_key: Option<crate::live::ScanKey> = None;
                loop {
                    if stop_prod.load(Ordering::SeqCst) {
                        return;
                    }
                    let need = slot_prod.lock().map(|g| g.is_none()).unwrap_or(false);
                    if need {
                        if let Some(aid) = crate::live::active_id() {
                            // 主档取半分辨率帧（`LIVE_SCAN_SCALE`）：解码成本 ∝ 像素数，
                            // 720p 逐帧从 5.5ms 降到 2.45ms，检出率实测持平。
                            if let Some(frame) =
                                crate::live::take_scan_frame(&aid, crate::live::LIVE_SCAN_SCALE)
                            {
                                if last_key.as_ref() != Some(&frame.key) {
                                    last_key = Some(frame.key);
                                    if let Ok(mut g) = slot_prod.lock() {
                                        *g = Some(frame);
                                    }
                                }
                            }
                        }
                    }
                    // 3ms 节拍：远快于流帧间隔（≥10ms），且空转成本可忽略。
                    std::thread::sleep(Duration::from_millis(3));
                }
            })
            .ok();
        LivePipe { slot, stop, handle }
    }

    /// 取走当前准备好的帧（无则 None）。
    fn take(&self) -> Option<crate::live::ScanFrame> {
        self.slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl Drop for LivePipe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
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
    /// 各显示器上次二维码命中位置（像素包围盒）——下轮先解此 ROI，命中则跳过全屏。
    /// 二维码在登录界面位置基本固定，命中后每轮解码范围可缩小一个数量级。
    last_hits: Vec<Option<(i32, i32, u32, u32)>>,
    /// 各显示器 ROI 连续未命中次数；超过上限即弃用该 ROI（应对界面布局变化）。
    roi_misses: Vec<u32>,
    unknown_qr_reported: u32,
    /// 每个票据的失败次数（超过上限后跳过，避免无限重试同一失效码）。
    failed_count: std::collections::HashMap<String, u32>,
    /// 上次「全幅精确解码」（快路径失败后的兜底）时刻；None = 尚未跑过（视为可跑）。
    /// 快路径（scan_density）对常见大码足够，但小码存在漏检可能——按节流间隔
    /// 周期性跑一次保真解码，把识别率下限锚在历史行为（见 decode_full_with_fallback）。
    last_exact_scan: Option<tokio::time::Instant>,
    /// 直播扫码：上次已解码帧的内容标识 (帧序号, 裁剪区)。60Hz 轮询下同一帧
    /// 会被取到多次，重复解码纯属浪费（流 30fps 时约占一半）——标识相同即
    /// 跳过解码（不改变「每个新帧都解码一次」的检测时效）。
    live_last_key: Option<crate::live::ScanKey>,
    /// 上次前台会话 id（检测切换以重置 live_last_key）。
    live_prev_id: Option<String>,
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
            last_hits: Vec::new(),
            roi_misses: Vec::new(),
            unknown_qr_reported: 0,
            failed_count: std::collections::HashMap::new(),
            last_exact_scan: None,
            live_last_key: None,
            live_prev_id: None,
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

/// ROI 命中位置外扩的边距（像素）：二维码静止时位置基本不变，留一点容差即可。
const ROI_PAD: i32 = 24;
/// ROI 连续未命中上限：超过即弃用该显示器 ROI（应对界面布局变化）。
const ROI_MAX_MISSES: u32 = 3;
/// ROI 最小边长：位置或尺寸异常（过小）时放弃 ROI，走全屏。
const ROI_MIN_SIDE: u32 = 32;

/// 「快扫失败 → 精确兜底」的节流间隔。
///
/// 背景（2026-10-11 实测，tests/bench_qr.rs）：快路径（scan_density(2,2)）
/// 无码帧耗时 2K ~73ms；保真扫描 ~128ms。若每帧都兜底，挂机 CPU 反而高于
/// 优化前；按此间隔周期性兜底，既把小码识别率下限锚在历史行为，又把挂机
/// 均值压在快路径水平（约每 10 轮多付一次精确扫描）。
const EXACT_FALLBACK_INTERVAL: Duration = Duration::from_millis(800);

/// 屏幕（桌面复制）扫码主档的降采样倍率（1/2）。
///
/// 依据（2026-10-13 真机截图复核，`tests/bench_qr_screen.rs`，**推翻旧结论**）：
/// `bench_qr.rs` 曾记「1/2 降采样两张截图均漏检、方案已否决」，但当时用的是
/// **面积平均（box）**降采样；改用**步长采样**后两张真机截图**均命中**，
/// 2569x1445 解码 **20.7ms → 6.2ms（-70%）**。
/// （直播侧同结论见 `live::LIVE_SCAN_SCALE`；1/3 在真机截图上漏检，不采用。）
///
/// 屏幕路径有 400ms 节流 + ROI 加速，本项收益是「进一步压低桌面模式占用」，
/// 识别率下限由全分辨率兜底（`FullSource::Screen`）锚定。
const SCREEN_SCAN_SCALE: u32 = 2;

/// 步长（最近邻）降采样：每 k×k 窗口取首点。
///
/// **必须用步长而非面积平均**：箱平均抹平模块边缘对比度，而 rxing 的
/// HybridBinarizer 依赖局部对比度——真机截图实测 1/2 箱平均两张均未命中、
/// 1/2 步长两张均命中；直播帧上 90px 码箱平均 0/8、步长 8/8。
fn stride_downsample(gray: &[u8], w: u32, h: u32, k: u32) -> (Vec<u8>, u32, u32) {
    let k = k.max(1);
    let (dw, dh) = ((w / k).max(1), (h / k).max(1));
    let mut out = Vec::with_capacity((dw as usize) * (dh as usize));
    for y in 0..dh {
        let row = (y * k) as usize * w as usize;
        for x in 0..dw {
            out.push(gray[row + (x * k) as usize]);
        }
    }
    (out, dw, dh)
}

/// 兜底档的**全分辨率**帧来源。
///
/// 主档为了压 CPU 用降采样帧（直播 1/2、屏幕 1/2），而降采样会漏掉小码
/// （实测 90px 以下），故周期性兜底必须回到全分辨率：**哪档取帧、哪档解码**
/// 要配套，不能拿降采样帧跑保真档（那是自欺：保真档再强也救不回已丢的细节）。
pub enum FullSource<'a> {
    /// 主档即全分辨率（或该路径不做降采样）。
    None,
    /// 直播：兜底时按会话 id 现取一帧全分辨率（800ms 一次，串行成本可接受）。
    Live(&'a str),
    /// 屏幕：本轮已抓到全分辨率灰度，直接复用。
    Screen(&'a [u8], u32, u32),
}

/// 把全分辨率坐标系的包围盒换算到主档（降采样）坐标系。
///
/// ROI 存的是「主档帧坐标系」的位置，后续在主档帧上裁剪；兜底档跑在
/// 全分辨率帧上，其 bounds 必须先除以倍率，否则 ROI 会锁到错误位置。
fn scale_bounds(mut hit: qr::QrHit, scale: u32) -> qr::QrHit {
    if scale > 1 {
        if let Some((x, y, w, h)) = hit.bounds {
            hit.bounds = Some((
                x / scale as i32,
                y / scale as i32,
                (w / scale).max(1),
                (h / scale).max(1),
            ));
        }
    }
    hit
}

/// 全幅解码（非 ROI 路径）：快路径优先，失败后按节流间隔用保真解码兜底。
///
/// 快路径 `scan_density(2,2)` 对模块尺寸较大的游戏登录码无识别损失（两张
/// 真机验证截图均命中，提速 ~43%）；小码/低对比目标存在漏检可能，故保留
/// 周期性精确兜底。兜底只在快路径**失败**后触发，命中路径零额外开销。
///
/// `scale`：主档帧的降采样倍率（兜底命中后需把 bounds 换算回该坐标系）。
/// `full`：兜底档的全分辨率帧来源（见 `FullSource`）。
fn decode_full_with_fallback(
    st: &mut LoopState,
    gray: &[u8],
    w: u32,
    h: u32,
    scale: u32,
    full: FullSource,
) -> Option<qr::QrHit> {
    if let Some(hit) = qr::decode_gray_hit_fast(gray, w, h) {
        return Some(hit);
    }
    let due = st
        .last_exact_scan
        .is_none_or(|t| t.elapsed() >= EXACT_FALLBACK_INTERVAL);
    if !due {
        return None;
    }
    st.last_exact_scan = Some(tokio::time::Instant::now());
    match full {
        // 直播兜底：现取一帧全分辨率（不走流水线，800ms 一次，串行成本可接受）。
        FullSource::Live(aid) => {
            if let Some(f) = crate::live::take_scan_frame(aid, 1) {
                if let Some(hit) = qr::decode_gray_hit(&f.gray, f.w, f.h) {
                    return Some(scale_bounds(hit, scale));
                }
            }
        }
        FullSource::Screen(g, fw, fh) => {
            if let Some(hit) = qr::decode_gray_hit(g, fw, fh) {
                return Some(scale_bounds(hit, scale));
            }
        }
        FullSource::None => {}
    }
    qr::decode_gray_hit(gray, w, h)
}

/// 从整帧裁剪出 (x,y,w,h) 子图，返回 (子图灰度, 宽, 高)。
/// 坐标越界自动收敛到帧内；裁剪后过小返回 None（退回全屏解码）。
/// 灰度 1 字节/像素（紧凑布局：行跨距 = width）。
pub fn crop_region(
    gray: &[u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = (x as i64 + w as i64).min(width as i64).max(0) as u32;
    let y1 = (y as i64 + h as i64).min(height as i64).max(0) as u32;
    if x1 <= x0 || y1 <= y0 || x1 - x0 < ROI_MIN_SIDE || y1 - y0 < ROI_MIN_SIDE {
        return None;
    }
    let cw = x1 - x0;
    let ch = y1 - y0;
    let mut out = Vec::with_capacity((cw * ch) as usize);
    for row in y0..y1 {
        let start = row as usize * width as usize + x0 as usize;
        let end = start + cw as usize;
        if end <= gray.len() {
            out.extend_from_slice(&gray[start..end]);
        } else {
            return None;
        }
    }
    Some((out, cw, ch))
}

/// ROI 未命中计数 +1；达上限即弃用该显示器 ROI（应对界面布局变化或
/// 锁到了非目标码）。下次解码回退全屏重新定位。
fn mark_roi_miss(st: &mut LoopState, idx: usize) {
    if st.roi_misses.len() <= idx {
        st.roi_misses.resize(idx + 1, 0);
    }
    st.roi_misses[idx] += 1;
    if st.roi_misses[idx] >= ROI_MAX_MISSES {
        if st.last_hits.len() > idx {
            st.last_hits[idx] = None;
        }
        st.roi_misses[idx] = 0;
    }
}

/// 解码一帧：优先在上次命中的 ROI 内解码，未命中回退全屏。
///
/// 游戏登录二维码在同一会话中位置基本固定，命中后每轮只需解一块小区域，
/// 解码耗时从整屏（~128ms）降到局部（通常 <40ms）。ROI 连续未命中达上限
/// 即弃用（应对界面布局变化），下次全屏重新定位。
///
/// ROI 内用保真配置（区域小、耗时低，优先稳住「已定位码」的复现）；
/// 全屏回退走快路径 + 周期性精确兜底（见 `decode_full_with_fallback`）。
///
/// 返回值第二项标记命中是否来自 ROI 分支。**本函数不锁定 ROI 位置**——
/// 屏幕上可能存在非目标二维码（如直播间关注码），若解出即锁定会永久错过
/// 真正的登录码。锁定由调用方在 `qr::extract` 确认是目标码后执行；
/// 非目标码若来自 ROI，调用方应补记 miss 以淘汰错误锁定。
fn decode_with_roi(
    st: &mut LoopState,
    idx: usize,
    gray: &[u8],
    w: u32,
    h: u32,
    scale: u32,
    full: FullSource,
) -> Option<(qr::QrHit, bool)> {
    if st.last_hits.len() <= idx {
        st.last_hits.resize(idx + 1, None);
        st.roi_misses.resize(idx + 1, 0);
    }
    // ROI 坐标存于主档（可能已降采样）坐标系，外扩边距同步缩放。
    let pad = (ROI_PAD / scale as i32).max(4);
    // 1) ROI 优先：上次命中位置外扩 pad，裁剪后单独解码。
    if let Some((x, y, rw, rh)) = st.last_hits[idx] {
        let rx = (x - pad).max(0);
        let ry = (y - pad).max(0);
        let pad2 = (pad * 2) as u32;
        if let Some((sub, sw, sh)) = crop_region(gray, w, h, rx, ry, rw + pad2, rh + pad2) {
            if let Some(hit) = qr::decode_gray_hit(&sub, sw, sh) {
                let full = hit
                    .bounds
                    .map(|(bx, by, bw, bh)| (bx + rx, by + ry, bw, bh));
                return Some((
                    qr::QrHit {
                        text: hit.text,
                        bounds: full,
                    },
                    true,
                ));
            }
        }
        // ROI 内未解出：累计失败，达上限则弃用该 ROI（下次全屏重新定位）。
        mark_roi_miss(st, idx);
    }
    // 2) 回退全屏：快路径（scan_density）优先，失败后周期性精确兜底；
    //    是否锁定 ROI 由调用方决定。
    let hit = decode_full_with_fallback(st, gray, w, h, scale, full)?;
    Some((hit, false))
}

/// 抽样生成画面灰度指纹；两帧指纹相同则认为画面未变化。
/// 输入已是灰度（1 字节/像素，行跨距 = width）。
fn thumbnail(gray: &[u8], width: u32, height: u32) -> Vec<u8> {
    let iw = width as usize;
    let mut out = Vec::new();
    let mut y = 0u32;
    while y < height {
        let mut x = 0u32;
        while x < width {
            let i = y as usize * iw + x as usize;
            if i < gray.len() {
                out.push(gray[i]);
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
    // 直播取帧流水线（与解码并行；随 stop 停止，函数返回时自动 join）。
    let live_pipe = LivePipe::start(stop.clone());

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

        // 直播源优先：只扫**当前前台（主画面）会话**——后台会话已暂停拉流，
        // 且避免后台画面误触确认。切换主画面后扫码目标随之切换。
        let active_live = crate::live::active_id();
        if let Some(aid) = active_live {
            // 切换前台会话（换房间）：重置去重键——不同会话的帧序号各自从 0 起，
            // 直接沿用旧键会误判「同一帧」而跳过新会话的首帧解码。
            if st.live_prev_id.as_deref() != Some(aid.as_str()) {
                st.live_prev_id = Some(aid.clone());
                st.live_last_key = None;
            }
            // 取帧走流水线（生产者已并行完成 YUV→灰度转换，本线程专注解码）。
            if let Some(frame) = live_pipe.take() {
                live_seen = true;
                // 同一帧去重：无新帧（标识相同）时跳过解码——重复解码同一像素
                // 不会更早发现二维码，只是白白烧 CPU。新帧必定解码一次，
                // 检测时效不变。
                if st.live_last_key == Some(frame.key) {
                    let wait = interval;
                    tokio::time::sleep(wait).await;
                    continue;
                }
                st.live_last_key = Some(frame.key);
                let (w, h) = (frame.w, frame.h);
                // zedbar 解码（唯一解码器）：快路径 + 周期性精确兜底
                // （与屏幕路径同一策略，快路径对直播 720p 帧提速 ~40%）。
                // 兜底档取全分辨率帧（主档是半分辨率），覆盖降质小码。
                let decoded = decode_full_with_fallback(
                    &mut st,
                    &frame.gray,
                    w,
                    h,
                    crate::live::LIVE_SCAN_SCALE,
                    FullSource::Live(&aid),
                )
                .map(|h| h.text);
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
        Err(_) => match crate::screenshot::capture_region_gray(rect.0, rect.1, rect.2, rect.3) {
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
    let thumb = thumbnail(&frame.gray, frame.w, frame.h);
    if st.last_thumbs.get(idx).and_then(|t| t.as_ref()) == Some(&thumb) {
        return false;
    }
    if st.last_thumbs.len() <= idx {
        st.last_thumbs.resize(idx + 1, None);
    }
    st.last_thumbs[idx] = Some(thumb);

    // 2) 解码：优先上次命中的 ROI，未命中回退全屏。
    //    主档跑在 1/2 步长降采样帧上（真机截图实测命中、-70% 耗时），
    //    全分辨率帧保留给周期性兜底（覆盖降采样会漏掉的小码）。
    let (sgray, sw, sh) = stride_downsample(&frame.gray, frame.w, frame.h, SCREEN_SCAN_SCALE);
    let Some((hit, from_roi)) = decode_with_roi(
        st,
        idx,
        &sgray,
        sw,
        sh,
        SCREEN_SCAN_SCALE,
        FullSource::Screen(&frame.gray, frame.w, frame.h),
    ) else {
        return false;
    };
    let text = hit.text;
    let Some(qrc) = qr::extract(&text) else {
        // 解出二维码但不是目标登录码：
        // - 若来自 ROI，说明 ROI 锁在了非目标码上（如直播间关注码），补记 miss
        //   使其最终被淘汰，避免永久错过真正的登录码；
        // - 来自全屏则不锁定（只有目标码才值得作为 ROI 锚点）。
        if from_roi {
            mark_roi_miss(st, idx);
        }
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
    // 确认为目标登录码：用其位置锁定 ROI，供后续轮次优先局部解码。
    if let Some(b) = hit.bounds {
        if st.last_hits.len() <= idx {
            st.last_hits.resize(idx + 1, None);
            st.roi_misses.resize(idx + 1, 0);
        }
        st.last_hits[idx] = Some(b);
        st.roi_misses[idx] = 0;
    }
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
