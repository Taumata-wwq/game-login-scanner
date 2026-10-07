//! 剪贴板图片读取 + 二维码解码 + ticket/tk 提取。
//!
//! 解码用 rqrr（纯 QR 实现）：亮度闭包直接从 RGB 采样，正常/反色各试一次；
//! 编码用 qrcodegen 生成 SVG。两者均为零/极轻依赖。

use std::cell::RefCell;

thread_local! {
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

/// 二维码在图像中的位置（像素，定位点外接框）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QrBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// 把四个角点收敛为图像内的外接框（负坐标夹到 0）；空点集返回 None。
fn bbox_from_points(points: &[(i32, i32)], width: u32, height: u32) -> Option<QrBox> {
    if points.is_empty() {
        return None;
    }
    let (mut min_x, mut min_y) = (i32::MAX, i32::MAX);
    let (mut max_x, mut max_y) = (0i32, 0i32);
    for (x, y) in points {
        min_x = min_x.min(*x);
        min_y = min_y.min(*y);
        max_x = max_x.max(*x);
        max_y = max_y.max(*y);
    }
    let w = width as i32;
    let h = height as i32;
    let x0 = min_x.clamp(0, w - 1) as u32;
    let y0 = min_y.clamp(0, h - 1) as u32;
    let x1 = (max_x + 1).clamp(1, w) as u32;
    let y1 = (max_y + 1).clamp(1, h) as u32;
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

/// 亮度采样：0 为黑、255 为白；`invert` 支持反色码（白码黑底，游戏登录界面常见）。
fn luminance_fn(rgb: &[u8], width: u32, invert: bool) -> impl Fn(usize, usize) -> u8 + '_ {
    move |x, y| {
        let i = (y * width as usize + x) * 3;
        let l =
            ((rgb[i] as u32 * 77 + rgb[i + 1] as u32 * 151 + rgb[i + 2] as u32 * 28) >> 8) as u8;
        if invert {
            255 - l
        } else {
            l
        }
    }
}

/// 解码单个 RGB 缓冲，返回 (文本, 外接框)。正常极性与反色各尝试一次。
fn decode_once(rgb: &[u8], width: u32, height: u32) -> Option<(String, Option<QrBox>)> {
    for invert in [false, true] {
        let lum = luminance_fn(rgb, width, invert);
        let mut img =
            rqrr::PreparedImage::prepare_from_greyscale(width as usize, height as usize, lum);
        let grids = img.detect_grids();
        let Some(grid) = grids.first() else { continue };
        let Ok((_, text)) = grid.decode() else {
            continue;
        };
        let corners: Vec<(i32, i32)> = grid.bounds.iter().map(|p| (p.x, p.y)).collect();
        return Some((text, bbox_from_points(&corners, width, height)));
    }
    None
}

/// 用 rqrr 解码 RGB 图片，返回 (文本, 二维码位置)。
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

/// zedbar（ZBar 算法）解码：rqrr 失败后的兜底，小二维码识别率更高。
///
/// 开启 retry_undecoded_regions：对检测到 finder 但未解出的区域自动裁剪+放大
/// 重试（等价「放大到二维码附近」）。返回 (文本, 位置)；位置来自 finder 区域，
/// 仅供 ROI 加速参考（无 finder 记录时为 None）。
pub fn decode_rgb_zedbar(rgb: &[u8], width: u32, height: u32) -> Option<(String, Option<QrBox>)> {
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
    let config = zedbar::DecoderConfig::new()
        .disable_all()
        .enable(zedbar::config::QrCode)
        .test_inverted(true) // 游戏登录码常见反色（白码黑底）
        .retry_undecoded_regions(true);
    let mut scanner = zedbar::Scanner::with_config(config);
    let result = scanner.scan(&mut img);
    let text = result
        .symbols()
        .first()
        .and_then(|s| s.data_string())?
        .to_string();
    // 位置：第一个 finder 区域（若有），仅用于 ROI 加速。
    let loc = result.finder_regions().first().map(|r| QrBox {
        x: r.x,
        y: r.y,
        w: r.width,
        h: r.height,
    });
    Some((text, loc))
}

/// 解码 RGB 图片，返回文本。
pub fn decode_rgb(rgb: &[u8], width: u32, height: u32) -> Option<String> {
    decode_rgb_located(rgb, width, height).map(|(text, _)| text)
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
