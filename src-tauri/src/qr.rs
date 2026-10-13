//! 剪贴板图片读取 + 二维码解码 + ticket/tk 提取。
//!
//! 解码用 zedbar（ZBar 算法纯 Rust 移植）：真实 2560x1440 游戏登录截图实测
//! 全屏 ~157ms 且 100% 命中。2026-10-08 移除 rqrr——其在真实游戏截图上
//! 降采样 + 全分辨率两条路径均 0 命中却耗时 ~560ms。
//! 所有游戏登录码均为白底黑码，无需反色处理。
//! 编码用 qrcodegen 生成 SVG。

/// 二维码类别。
///
/// 注意：崩坏3 PC 客户端（B 服与官服）的登录二维码都是
/// `qr_code_in_game.html?ticket=` 形态，平台无法从 URL 区分，
/// 需在确认阶段按已登录账号自动路由（见 scan.rs）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrKind {
    /// 米哈游游戏登录码（`qr_code_in_game.html?ticket=`），可用任一渠道账号确认。
    InGame,
    /// 米游社通行证码（`mobile.html?tk=`），即本工具生成的「手机扫码」码，
    /// 游戏端无法用它确认，屏幕扫描时忽略。
    Passport,
    /// 鹰角游戏登录码（`hypergryph://scan_login?scanId=`），用鹰角账号确认。
    Hypergryph,
}

/// 解析出的可用登录码。
#[derive(Debug, Clone)]
pub struct QrCode {
    pub kind: QrKind,
    pub ticket: String,
    /// 游戏业务键（`biz_key`，如 `bh3_cn`），决定 `combo/panda` 端点前缀。
    /// 仅游戏登录码有；通行证码为空。
    pub biz_key: String,
    pub raw: String,
}

/// 从二维码 URL 判定可用码并提取票据：
/// - 含 `qr_code_in_game.html` 且 `ticket=` → 游戏登录码
/// - 含 `mobile.html` 且 `tk=` → 米游社通行证码
///
/// 参考 `bh3_login_simulation` 的 `QRScanner.parseUrl`：游戏码还会携带
/// `biz_key`（决定端点前缀）；`app_id` / `app_name` 与确认流程无关，不解析。
pub fn extract(url: &str) -> Option<QrCode> {
    if url.contains("qr_code_in_game.html") {
        if let Some(ticket) = extract_param(url, "ticket=") {
            let biz_key = extract_param(url, "biz_key=")
                .unwrap_or_else(|| crate::constants::BH3_DEFAULT_BIZ_KEY.to_string());
            return Some(QrCode {
                kind: QrKind::InGame,
                ticket,
                biz_key,
                raw: url.to_string(),
            });
        }
    }
    if url.contains("mobile.html") {
        if let Some(tk) = extract_param(url, "tk=") {
            return Some(QrCode {
                kind: QrKind::Passport,
                ticket: tk,
                biz_key: String::new(),
                raw: url.to_string(),
            });
        }
    }
    // 鹰角（明日方舟 / 终末地）：hypergryph://scan_login?scanId=xxx
    if url.contains("hypergryph://scan_login") {
        if let Some(scan_id) = extract_param(url, "scanId=") {
            return Some(QrCode {
                kind: QrKind::Hypergryph,
                ticket: scan_id,
                biz_key: String::new(),
                raw: url.to_string(),
            });
        }
    }
    None
}

fn extract_param(url: &str, key: &str) -> Option<String> {
    for part in url.split(['?', '&']) {
        if let Some(rest) = part.strip_prefix(key) {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// RGBA 字节转 RGB。
pub fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

/// RGB 字节转灰度（BT.601 加权整数实现，`(77R+151G+28B)>>8`）。
/// 扫码热路径已直接产出灰度（见 `decode_gray_hit`），本函数供一次性路径
/// （剪贴板图片、测试）与仍持有 RGB 缓冲的调用方使用；结果与历史实现逐位一致。
pub fn rgb_to_gray(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let n = width as usize * height as usize;
    rgb.as_chunks::<3>()
        .0
        .iter()
        .take(n)
        .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 151 + p[2] as u32 * 28) >> 8) as u8)
        .collect()
}

/// 解码结果：文本 + 二维码在图像中的轴对齐包围盒（像素坐标，用于 ROI 加速）。
///
/// bounds 为 None 表示 zedbar 未记录位置（对 QR 码通常都有）。
#[derive(Debug, Clone)]
pub struct QrHit {
    pub text: String,
    /// (x, y, width, height)，相对传入图像左上角。
    pub bounds: Option<(i32, i32, u32, u32)>,
}

/// zedbar（ZBar 算法纯 Rust 移植）解码灰度图，返回文本与位置。
///
/// **扫码热路径入口**：QR 识别只需亮度信息，扫描链路（桌面复制 / 直播 YUV）
/// 直接产出灰度缓冲，避免「RGB 三通道写入 + 二次 RGB→灰度遍历」的重复开销
/// （1080p 每帧省约 2/3 写入量与一次全图转换）。
pub fn decode_gray_hit(gray: &[u8], width: u32, height: u32) -> Option<QrHit> {
    decode_impl(gray, width, height, false)
}

/// 快配置灰度解码：`scan_density(2,2)` 隔行扫描（每 2 行/列抽 1）。
///
/// 2026-10-11 实测（tests/bench_qr.rs，两张真机验证截图）：
/// - 2K 整屏 2569×1445：128ms → 73ms（-43%），命中；
/// - 720p 1282×747：32ms → 18ms（-45%），命中。
///
/// 隔行只减少扫描密度、不重采样图像（不同于「先降采样成小图再解码」——实测
/// 1/2 面积平均降采样会导致两张截图均漏检，已否决该方案），对游戏登录码这种
/// 模块尺寸较大的目标无识别损失；小码漏检风险由调用方「周期性精确兜底」覆盖
/// （见 scan.rs `decode_full_with_fallback`）。
pub fn decode_gray_hit_fast(gray: &[u8], width: u32, height: u32) -> Option<QrHit> {
    decode_impl(gray, width, height, true)
}

/// RGB 便捷入口（内部转灰度）：剪贴板等一次性路径用，勿在热路径调用。
pub fn decode_rgb_hit(rgb: &[u8], width: u32, height: u32) -> Option<QrHit> {
    if width == 0 || height == 0 {
        return None;
    }
    decode_impl(&rgb_to_gray(rgb, width, height), width, height, false)
}

/// 同 `decode_rgb_hit`，快配置。
pub fn decode_rgb_hit_fast(rgb: &[u8], width: u32, height: u32) -> Option<QrHit> {
    if width == 0 || height == 0 {
        return None;
    }
    decode_impl(&rgb_to_gray(rgb, width, height), width, height, true)
}

/// 解码配置：快路径 `scan_density(2,2)`；**内置的无界小码重试始终关闭**，
/// 改由 `bounded_retry` 做限额重试（见其文档）。
fn base_config(fast: bool) -> zedbar::DecoderConfig {
    // 所有游戏登录码均为白底黑码（正常极性），无需 test_inverted。
    // 0.7 起 DecoderConfig::new() 默认全禁，显式 enable 二维码即可。
    let config = zedbar::DecoderConfig::new()
        .enable(zedbar::config::QrCode)
        .position_tracking(true)
        .retry_undecoded_regions(false);
    if fast {
        config.scan_density(2, 2)
    } else {
        config
    }
}

/// 有界小码重试：只对**前 N 个候选区域**做裁剪放大重扫，且受面积/耗时双重预算约束。
///
/// 背景（2026-10-13 实测）：zedbar 内置 `retry_undecoded_regions(true)` 会对全部
/// 候选区域 × 3 档放大（2x/4x/6x）逐个重扫（内部上限 16 区域），细节密集的游戏/
/// 直播画面下候选常达数十个 → 单帧解码 577.9ms（1080p）/ 97.2ms（720p），
/// 而关闭后仅 47.5ms / 38.9ms——它是扫码 CPU 与单帧延迟抖动的主要来源。
///
/// 有界策略在保留「小码兜底」的前提下把最坏耗时压到可预测范围：
/// - 候选按面积降序，只取前 `MAX_RETRY_REGIONS` 个；
/// - 跳过大区域（>10% 画面，与内置实现一致：大区域多为 1D 码误报）；
/// - 放大后像素数超 `RETRY_MAX_UPSCALED_PX` 的档位直接跳过（防止 4x 一个
///   大区域产生 16M 像素的巨图重扫）；
/// - 每步前检查总耗时预算 `RETRY_TIME_BUDGET`，超时立即停止。
///
/// 重试用与主扫描相同的配置（同 density），与内置实现语义一致。
const MAX_RETRY_REGIONS: usize = 2;
/// 重试放大倍率（内置实现的前两档；6x 收益最低且放大后像素最多，省略）。
const RETRY_SCALES: &[u32] = &[2, 4];
/// 重试总耗时预算：超出即停止（本帧按未命中处理，下一帧再试）。
const RETRY_TIME_BUDGET: std::time::Duration = std::time::Duration::from_millis(60);
/// 候选区域最小边长：小于此值视为噪声碎片，放大也无意义。
const RETRY_MIN_SIDE: u32 = 16;
/// 候选区域面积占画面比例上限（与 zedbar 内置重试一致）。
const RETRY_MAX_REGION_RATIO: u64 = 10; // 1/10
/// 放大后单图像素上限（约 2MP：单次重扫耗时与 1080p 主扫描同量级）。
const RETRY_MAX_UPSCALED_PX: u64 = 2_000_000;

fn bounded_retry(
    src: &zedbar::Image,
    regions: &[zedbar::FinderRegion],
    fast: bool,
) -> Option<QrHit> {
    if regions.is_empty() {
        return None;
    }
    let (iw, ih) = (src.width(), src.height());
    let img_area = iw as u64 * ih as u64;
    let area_limit = (img_area / RETRY_MAX_REGION_RATIO).max(1);
    let t0 = std::time::Instant::now();
    // 候选按面积降序：面积大者更可能是真实二维码（内置实现按置信度排序返回，
    // 这里用面积做等价近似，避免依赖内部排序细节）。
    let mut cands: Vec<zedbar::FinderRegion> = regions.to_vec();
    cands.sort_by_key(|r| std::cmp::Reverse(r.width as u64 * r.height as u64));
    for region in cands.into_iter().take(MAX_RETRY_REGIONS) {
        if t0.elapsed() > RETRY_TIME_BUDGET {
            break;
        }
        if region.width < RETRY_MIN_SIDE || region.height < RETRY_MIN_SIDE {
            continue;
        }
        if region.width as u64 * region.height as u64 > area_limit {
            continue;
        }
        // 外扩 50% 作为静区（与 zedbar 内置重试一致：二维码解码需要静区）。
        let pad_x = region.width / 2;
        let pad_y = region.height / 2;
        let cx = region.x.saturating_sub(pad_x);
        let cy = region.y.saturating_sub(pad_y);
        let cw = (region.width + 2 * pad_x).min(iw.saturating_sub(cx));
        let ch = (region.height + 2 * pad_y).min(ih.saturating_sub(cy));
        let Some(cropped) = src.crop(cx, cy, cw, ch) else {
            continue;
        };
        for &scale in RETRY_SCALES {
            if t0.elapsed() > RETRY_TIME_BUDGET {
                break;
            }
            // 放大后像素上限：超限的档位跳过（4x 对大区域会产生巨图）。
            let up_px = cw as u64 * ch as u64 * (scale as u64) * (scale as u64);
            if up_px > RETRY_MAX_UPSCALED_PX {
                continue;
            }
            let Some(mut up) = cropped.upscale(scale) else {
                continue;
            };
            let mut scanner = zedbar::Scanner::with_config(base_config(fast));
            let res = scanner.scan(&mut up);
            let Some(sym) = res.symbols().first() else {
                continue;
            };
            let Some(text) = sym.data_string() else {
                continue;
            };
            // 坐标映射回原图：放大图坐标 ÷ 倍率 + 裁剪原点。
            let bounds = sym.bounds().map(|b| {
                (
                    b.x / scale as i32 + cx as i32,
                    b.y / scale as i32 + cy as i32,
                    (b.width / scale).max(1),
                    (b.height / scale).max(1),
                )
            });
            return Some(QrHit {
                text: text.to_string(),
                bounds,
            });
        }
    }
    None
}

fn decode_impl(gray: &[u8], width: u32, height: u32, fast: bool) -> Option<QrHit> {
    if width == 0 || height == 0 {
        return None;
    }
    let mut img = zedbar::Image::from_gray(gray, width, height).ok()?;
    let mut scanner = zedbar::Scanner::with_config(base_config(fast));
    // 扫描不写像素（仅内部走线缓冲），故同一 Image 可直接用于后续的有界重试。
    let result = scanner.scan(&mut img);
    if let Some(sym) = result.symbols().first() {
        let text = sym.data_string()?.to_string();
        let bounds = sym.bounds().map(|b| (b.x, b.y, b.width, b.height));
        return Some(QrHit { text, bounds });
    }
    // 主扫描未命中：对候选区域做有界重试（小码兜底，见 bounded_retry）。
    bounded_retry(&img, result.finder_regions(), fast)
}

/// 兼容旧签名：只返回文本（保真配置；剪贴板等一次性路径用）。
pub fn decode_rgb(rgb: &[u8], width: u32, height: u32) -> Option<String> {
    decode_rgb_hit(rgb, width, height).map(|h| h.text)
}

/// 读取剪贴板图片（转为 RGB），空/非图返回 None。
pub fn read_clipboard_image() -> Option<(Vec<u8>, u32, u32)> {
    let (rgba, w, h) = crate::clipboard::read_image_rgba()?;
    let rgb = rgba_to_rgb(&rgba);
    Some((rgb, w, h))
}

// ---------- 二维码生成 ----------

/// 把文本编码为二维码 SVG，供前端直接内嵌展示。
/// MARGIN=0：白边由前端容器的 4px padding 提供（默认 4 模块静区太宽）。
pub fn qr_svg(text: &str) -> Result<String, String> {
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Low)
        .map_err(|e| format!("二维码编码失败: {e:?}"))?;
    let n = qr.size(); // 模块数（i32）
    let mut path = String::new();
    for y in 0..n {
        for x in 0..n {
            if qr.get_module(x, y) {
                path.push_str(&format!("M{x},{y}h1v1h-1z"));
            }
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {n} {n}\" shape-rendering=\"crispEdges\">\
<rect width=\"{n}\" height=\"{n}\" fill=\"#ffffff\"/>\
<path d=\"{path}\" fill=\"#111111\"/></svg>"
    ))
}
