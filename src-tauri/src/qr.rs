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

/// zedbar（ZBar 算法纯 Rust 移植）解码 RGB 图片，返回文本。
///
/// 本项目唯一解码器（2026-10-08 移除 rqrr）。真实 2560x1440 游戏登录截图实测：
/// 全屏 ~157ms 且 100% 命中。
///
/// 开启 retry_undecoded_regions：对检测到 finder 但未解出的区域自动裁剪+放大
/// 重试（等价「放大到二维码附近」），提升小二维码识别率。
///
/// 注：不返回二维码位置——zedbar 只在**未解码**时记录 finder 区域，成功解码的
/// 坐标不公开（Symbol.pts 为 crate 内部）。全屏解码已足够快，无需 ROI 加速。
pub fn decode_rgb(rgb: &[u8], width: u32, height: u32) -> Option<String> {
    if width == 0 || height == 0 {
        return None;
    }
    // zedbar 只吃灰度：BT.601 亮度。
    let gray: Vec<u8> = rgb
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 151 + p[2] as u32 * 28) >> 8) as u8)
        .collect();
    let mut img = zedbar::Image::from_gray(&gray, width, height).ok()?;
    // 所有游戏登录码均为白底黑码（正常极性），无需 test_inverted。
    let config = zedbar::DecoderConfig::new()
        .disable_all()
        .enable(zedbar::config::QrCode)
        .retry_undecoded_regions(true);
    let mut scanner = zedbar::Scanner::with_config(config);
    let result = scanner.scan(&mut img);
    result
        .symbols()
        .first()
        .and_then(|s| s.data_string())
        .map(|s| s.to_string())
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
