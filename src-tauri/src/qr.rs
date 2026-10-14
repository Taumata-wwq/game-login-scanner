//! 剪贴板图片读取 + 二维码解码 + ticket/tk 提取。
//!
//! 解码用 rxing（ZXing 的纯 Rust 移植，Apache-2.0；2026-10-13 由 zedbar 换代，
//! 720p 直播画面 11.3ms → 4.5ms/帧）。更早的 rqrr 已于 2026-10-08 移除——
//! 其在真实游戏截图上降采样 + 全分辨率两条路径均 0 命中却耗时 ~560ms。
//! 所有游戏登录码均为白底黑码，无需反色处理。
//!
//! 生产主档跑在 **1/2 步长降采样**帧上（见 `scan::SCREEN_SCAN_SCALE` 与
//! `live::LIVE_SCAN_SCALE`）：解码成本 ∝ 像素数，降采样省 ~70%，且实测
//! 检出率与全分辨率持平——**但降采样必须用步长采样而非面积平均**（箱平均
//! 抹平模块边缘对比度会漏检，见 tests/bench_qr_screen.rs）。
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

/// RGBA 字节转灰度（BT.601 加权整数实现，`(77R+151G+28B)>>8`）。
///
/// 扫码热路径本就直出灰度（见 `decode_gray_hit`），本函数只服务**一次性路径**
/// （剪贴板图片）；权重与桌面复制 / 直播取帧侧完全一致。
pub fn rgba_to_gray(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let n = width as usize * height as usize;
    rgba.as_chunks::<4>()
        .0
        .iter()
        .take(n)
        .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 151 + p[2] as u32 * 28) >> 8) as u8)
        .collect()
}

/// 解码结果：文本 + 二维码在图像中的轴对齐包围盒（像素坐标，用于 ROI 加速）。
///
/// bounds 为 None 表示解码器未记录位置（对 QR 码通常都有）。
#[derive(Debug, Clone)]
pub struct QrHit {
    pub text: String,
    /// (x, y, width, height)，相对传入图像左上角。
    pub bounds: Option<(i32, i32, u32, u32)>,
}

/// 解码灰度图（rxing：ZXing 纯 Rust 移植），返回文本与位置。
///
/// **扫码热路径入口**：QR 识别只需亮度信息，扫描链路（桌面复制 / 直播 YUV）
/// 直接产出灰度缓冲，避免「RGB 三通道写入 + 二次 RGB→灰度遍历」的重复开销
/// （1080p 每帧省约 2/3 写入量与一次全图转换）。
///
/// 解码内核为 rxing（QR-only + HybridBinarizer），2026-10-13 实测 4.5ms/帧（720p）。
pub fn decode_gray_hit(gray: &[u8], width: u32, height: u32) -> Option<QrHit> {
    decode_impl(gray, width, height, false)
}

/// 快档灰度解码（生产逐帧路径）：rxing QR-only 常规模式。
///
/// 2026-10-13 实测：真实 720p 直播画面 **4.5ms/帧**（旧 zedbar `scan_density(2,2)`
/// 为 11.3ms）；检出矩阵与 70~200px 小码回归样本全过。小码/模糊场景的漏检风险由
/// 调用方的周期性兜底覆盖（见 scan.rs `decode_full_with_fallback` 与 `decode_gray_hit`）。
pub fn decode_gray_hit_fast(gray: &[u8], width: u32, height: u32) -> Option<QrHit> {
    decode_impl(gray, width, height, true)
}

/// 解码实现：rxing（ZXing 纯 Rust 移植，Apache-2.0）。
///
/// 选型沿革（2026-10-13 实测，24 帧真实 720p 直播画面 + 合成小码回归样本）：
/// - 旧实现 zedbar 0.7（ZBar 算法移植）：主扫描 **11.3ms/帧**（无码画面），
///   小码兜底依赖「候选区域裁剪放大重扫」（内置版 577.9ms 已改为有界版 +12ms）；
/// - 新实现 rxing 0.9：**4.5ms/帧（-60%）**，检出矩阵（180/300/120/90px，含左右
///   边缘位置）**6/6 全过**，70~200px 小码回归全过；许可证由 LGPL-3.0 换为 Apache-2.0；
/// - 对照：zxing-cpp 0.5（C++，默认选项含旋转/降采样多遍扫描）26.7ms —— 明显更慢，
///   且其 Rust 封装未暴露 tryRotate/tryDownscale 等调优开关，不采用。
///
/// 两档：
/// - `fast = true`：常规逐帧扫描（QR-only + HybridBinarizer 自适应二值化）；
/// - `fast = false`：**TRY_HARDER** 档（ZXing 的"多花时间换召回"模式），
///   由调用方（scan.rs）按周期节流触发，替代旧 zedbar 的有界重试。
fn decode_impl(gray: &[u8], width: u32, height: u32, fast: bool) -> Option<QrHit> {
    use rxing::Reader as _;
    if width == 0 || height == 0 {
        return None;
    }
    // 全帧灰度 → 亮度源（零拷贝借用）+ 混合二值化（ZXing 默认，局部自适应）。
    let src = rxing::Luma8Source::new_with_slice(gray, width, height).ok()?;
    let mut bitmap = rxing::BinaryBitmap::new(rxing::common::HybridBinarizer::new(src));
    // 只找二维码（关闭其它码制可显著减少候选扫描，与生产场景一致）。
    let mut hints = rxing::DecodeHints::default().with(rxing::DecodeHintValue::PossibleFormats(
        std::collections::HashSet::from([rxing::BarcodeFormat::QR_CODE]),
    ));
    if !fast {
        hints = hints.with(rxing::DecodeHintValue::TryHarder(true));
    }
    let mut reader = rxing::MultiFormatReader::default();
    let res = reader.decode_with_hints(&mut bitmap, &hints).ok()?;
    let text = res.getText().to_string();
    Some(QrHit {
        text,
        bounds: result_bounds(&res),
    })
}

/// 由解码结果点集计算轴对齐包围盒（供 scan.rs 的 ROI 加速使用）。
/// 无点集（极少见）时返回 None —— 调用方会退化为全屏扫描。
fn result_bounds(res: &rxing::RXingResult) -> Option<(i32, i32, u32, u32)> {
    let pts = res.getPoints();
    if pts.is_empty() {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut any = false;
    for p in pts {
        if !p.x.is_finite() || !p.y.is_finite() {
            continue;
        }
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
        any = true;
    }
    if !any {
        return None;
    }
    let w = (x1 - x0).max(1.0) as u32;
    let h = (y1 - y0).max(1.0) as u32;
    Some((x0.max(0.0) as i32, y0.max(0.0) as i32, w, h))
}

/// 读取剪贴板图片并转为灰度（空/非图返回 None）。
pub fn read_clipboard_gray() -> Option<(Vec<u8>, u32, u32)> {
    let (rgba, w, h) = crate::clipboard::read_image_rgba()?;
    Some((rgba_to_gray(&rgba, w, h), w, h))
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
