//! 剪贴板图片读取 + 二维码解码 + ticket/tk 提取。

use std::cell::RefCell;

thread_local! {
    /// 解码用 0xRRGGBB 像素缓冲（线程内复用，避免每帧大块分配）。
    static DECODE_PIXELS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    /// 2×2 降采样输出缓冲（线程内复用）。
    static DOWNSCALE_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

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

/// 构造解码提示字典：TRY_HARDER 提升小码识别；ALSO_INVERTED 支持
/// 白码黑底（反色）二维码——游戏登录界面常见。
fn decode_hints() -> rxing::DecodingHintDictionary {
    let mut hints = rxing::DecodingHintDictionary::new();
    hints.insert(
        rxing::DecodeHintType::TRY_HARDER,
        rxing::DecodeHintValue::TryHarder(true),
    );
    hints.insert(
        rxing::DecodeHintType::ALSO_INVERTED,
        rxing::DecodeHintValue::AlsoInverted(true),
    );
    hints
}

/// 二维码在图像中的位置（像素，定位点外接框）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QrBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// 把定位点集收敛为图像内的外接框；无定位点返回 None。
fn bbox_from_points(points: &[rxing::Point], width: u32, height: u32) -> Option<QrBox> {
    if points.is_empty() {
        return None;
    }
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for p in points {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let x0 = min_x.floor().max(0.0) as u32;
    let y0 = min_y.floor().max(0.0) as u32;
    let x1 = (max_x.ceil() as u32).min(width);
    let y1 = (max_y.ceil() as u32).min(height);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(QrBox {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

/// 外扩矩形并夹紧到图像边界（至少保留 1px）。
pub fn expand_box(b: QrBox, margin: u32, width: u32, height: u32) -> QrBox {
    let x = b.x.saturating_sub(margin).min(width.saturating_sub(1));
    let y = b.y.saturating_sub(margin).min(height.saturating_sub(1));
    let w = (b.w + margin * 2).min(width - x).max(1);
    let h = (b.h + margin * 2).min(height - y).max(1);
    QrBox { x, y, w, h }
}

/// 裁剪 RGB 缓冲，返回 (RGB, 宽, 高)。
pub fn crop_rgb(rgb: &[u8], width: u32, height: u32, b: QrBox) -> Option<(Vec<u8>, u32, u32)> {
    if width == 0 || height == 0 || b.x >= width || b.y >= height {
        return None;
    }
    let w = b.w.min(width - b.x);
    let h = b.h.min(height - b.y);
    if w == 0 || h == 0 {
        return None;
    }
    let iw = width as usize;
    let x0 = b.x as usize;
    let mut out = Vec::with_capacity(w as usize * h as usize * 3);
    for y in b.y..b.y + h {
        let start = ((y as usize * iw) + x0) * 3;
        let end = start + w as usize * 3;
        out.extend_from_slice(&rgb[start..end]);
    }
    Some((out, w, h))
}

/// 解码单个 RGB 缓冲，返回 (文本, 定位框)。
/// 像素缓冲线程内复用：只在 `DECODE_PIXELS` 里重建 0xRRGGBB 像素，
/// 不再每帧分配 w*h 的 Vec<u32>（1080p 单帧约 8MB）。
fn decode_once(rgb: &[u8], width: u32, height: u32) -> Option<(String, Option<QrBox>)> {
    use rxing::common::HybridBinarizer;
    use rxing::{BinaryBitmap, MultiFormatReader, RGBLuminanceSource, Reader};

    DECODE_PIXELS.with(|cell| {
        let mut pixels = cell.borrow_mut();
        pixels.clear();
        pixels.reserve(width as usize * height as usize);
        pixels.extend(
            rgb.as_chunks::<3>()
                .0
                .iter()
                .map(|p| ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | (p[2] as u32)),
        );
        let source = RGBLuminanceSource::new_with_width_height_pixels(
            width as usize,
            height as usize,
            &pixels,
        );
        let mut bitmap = BinaryBitmap::new(HybridBinarizer::new(source));
        let mut reader = MultiFormatReader::default();
        reader
            .decode_with_hints(&mut bitmap, &decode_hints())
            .ok()
            .map(|r| {
                let text = r.getText().to_string();
                let loc = bbox_from_points(r.getRXingResultPoints(), width, height);
                (text, loc)
            })
    })
}

/// 用 rxing 解码 RGB 图片，返回 (文本, 二维码位置)。
/// 大图先降采样、失败再用原尺寸；小图失败时放大重试。
/// 位置坐标始终已换算回原图尺度。
pub fn decode_rgb_located(rgb: &[u8], width: u32, height: u32) -> Option<(String, Option<QrBox>)> {
    decode_rgb_downscaled(rgb, width, height).or_else(|| decode_rgb_fullres(rgb, width, height))
}

/// 2×2 降采样快路径：仅对大图（任一边 ≥ 1600px）生效。
/// 降采样输出写入线程复用缓冲，随后就地解码（无每帧分配）。
pub fn decode_rgb_downscaled(
    rgb: &[u8],
    width: u32,
    height: u32,
) -> Option<(String, Option<QrBox>)> {
    if width == 0 || height == 0 || (width < 1600 && height < 1600) {
        return None;
    }
    let decoded = DOWNSCALE_BUF.with(|cell| {
        let mut small = cell.borrow_mut();
        small.clear();
        let w = (width / 2).max(1) as usize;
        let h = (height / 2).max(1) as usize;
        small.reserve(w * h * 3);
        let iw = width as usize;
        let ih = height as usize;
        for y in 0..h {
            let y0 = y * 2;
            let y1 = (y0 + 1).min(ih - 1);
            for x in 0..w {
                let x0 = x * 2;
                let x1 = (x0 + 1).min(iw - 1);
                for c in 0..3 {
                    let a = rgb[(y0 * iw + x0) * 3 + c] as u32;
                    let b = rgb[(y0 * iw + x1) * 3 + c] as u32;
                    let d = rgb[(y1 * iw + x0) * 3 + c] as u32;
                    let e = rgb[(y1 * iw + x1) * 3 + c] as u32;
                    small.push(((a + b + d + e) / 4) as u8);
                }
            }
        }
        decode_once(&small, w as u32, h as u32)
    });
    let (text, loc) = decoded?;
    // 降采样图坐标 ×2 回原图
    let loc = loc.map(|b| QrBox {
        x: b.x * 2,
        y: b.y * 2,
        w: b.w * 2,
        h: b.h * 2,
    });
    Some((text, loc))
}

/// 原尺寸解码；小图（任一边 < 300px）失败时放大 2 倍重试一次。
pub fn decode_rgb_fullres(rgb: &[u8], width: u32, height: u32) -> Option<(String, Option<QrBox>)> {
    if width == 0 || height == 0 {
        return None;
    }
    if let Some((text, loc)) = decode_once(rgb, width, height) {
        return Some((text, loc));
    }
    if width < 300 || height < 300 {
        let scale = 2usize;
        let (nw, nh) = (width as usize * scale, height as usize * scale);
        if let (Some(nw), Some(nh)) = (
            u32::try_from(nw).ok().filter(|&v| v <= 8192),
            u32::try_from(nh).ok().filter(|&v| v <= 8192),
        ) {
            let mut upscaled = Vec::with_capacity(rgb.len() * scale * scale);
            for y in 0..height as usize {
                for _ in 0..scale {
                    for x in 0..width as usize {
                        let px =
                            &rgb[(y * width as usize + x) * 3..(y * width as usize + x) * 3 + 3];
                        for _ in 0..scale {
                            upscaled.extend_from_slice(px);
                        }
                    }
                }
            }
            if let Some((text, loc)) = decode_once(&upscaled, nw, nh) {
                // 放大图坐标 ÷2 回原图
                let loc = loc.map(|b| QrBox {
                    x: b.x / 2,
                    y: b.y / 2,
                    w: b.w / 2,
                    h: b.h / 2,
                });
                return Some((text, loc));
            }
        }
    }
    None
}

/// 用 rxing 解码 RGB 图片，返回文本。
pub fn decode_rgb(rgb: &[u8], width: u32, height: u32) -> Option<String> {
    decode_rgb_located(rgb, width, height).map(|(text, _)| text)
}

/// 读取剪贴板图片（转为 RGB），空/非图返回 None。
pub fn read_clipboard_image() -> Option<(Vec<u8>, u32, u32)> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let image = clipboard.get_image().ok()?;
    let w = image.width as u32;
    let h = image.height as u32;
    let rgb = rgba_to_rgb(&image.bytes);
    Some((rgb, w, h))
}

// ---------- 二维码生成 ----------

/// 把文本编码为二维码 SVG，供前端直接内嵌展示。
/// MARGIN=0：白边由前端容器的 4px padding 提供（默认 4 模块静区太宽）。
pub fn qr_svg(text: &str) -> Result<String, String> {
    use rxing::{BarcodeFormat, MultiFormatWriter, Writer};

    let size = 33 * 8; // 足够容纳最长 ticket URL 的边长
    let mut hints = rxing::EncodingHintDictionary::new();
    hints.insert(
        rxing::EncodeHintType::MARGIN,
        rxing::EncodeHintValue::Margin("0".to_string()),
    );
    let matrix = MultiFormatWriter
        .encode_with_hints(text, &BarcodeFormat::QR_CODE, size, size, &hints)
        .map_err(|e| format!("二维码编码失败: {e}"))?;
    let (w, h) = (matrix.getWidth(), matrix.getHeight());
    let mut path = String::new();
    for y in 0..h {
        for x in 0..w {
            if matrix.get(x, y) {
                path.push_str(&format!("M{x},{y}h1v1h-1z"));
            }
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" shape-rendering=\"crispEdges\">\
<rect width=\"{w}\" height=\"{h}\" fill=\"#ffffff\"/>\
<path d=\"{path}\" fill=\"#111111\"/></svg>"
    ))
}
