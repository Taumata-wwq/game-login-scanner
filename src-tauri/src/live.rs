//! 直播流捕获：B站 http-flv → FLV 解复用 → openh264 H.264 解码 → RGBA 帧总线。
//!
//! 低延迟链路：B站 API（免登录）解析出 http-flv 拉流地址 → 专用流式连接边下边解
//! （逐 Tag 解复用，解码即存共享帧）。内置追帧机制（时间戳 vs 墙钟测积压，落后即丢弃
//! 至关键帧），延迟稳定约 1.2 秒，长跑不漂移。
//! 共享帧供两条消费路径：
//! - 预览：`crop_frame` 按前端视口裁剪降采样（等价屏幕预览协议）；
//! - 扫码：`take_scan_frame` 取新帧走既有二维码识别管线（seq 去重）。

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use openh264::formats::YUVSource;

use crate::http::header_utf8;

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

// ---------- 流地址解析 ----------

/// 解析 B站直播间为 http-flv 拉流地址，返回 (url, referer)。
async fn resolve_stream(room: &str, qn: u32) -> Result<(String, String), String> {
    {
        {
            let headers = vec![
                ("User-Agent".to_string(), header_utf8(BROWSER_UA)),
                (
                    "Referer".to_string(),
                    format!("https://live.bilibili.com/{room}"),
                ),
            ];
            let url = format!(
                "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={room}&protocol=0,1&format=0,2&codec=0,1&qn={qn}&platform=web&ptype=16"
            );
            let resp = crate::http::get(&url, headers.clone())
                .await
                .map_err(|e| format!("B站直播接口请求失败: {e}"))?;
            let v: Value = serde_json::from_str(&resp.body).map_err(|_| {
                format!(
                    "B站直播接口响应非 JSON: {}",
                    &resp.body[..resp.body.len().min(120)]
                )
            })?;
            if v.get("code").and_then(|x| x.as_i64()) != Some(0) {
                let msg = v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("接口错误");
                return Err(format!("B站直播间 {room} 获取失败: {msg}"));
            }
            // 未开播快速失败（避免等 15s 首帧超时）。
            if let Some(ls) = v["data"]["live_status"].as_i64() {
                if ls != 1 {
                    return Err(format!("B站直播间 {room} 未开播"));
                }
            }
            // 首选：v2 结构化流（url_list 非空时）。
            if let Some(u) = parse_bili_flv(&v) {
                return Ok((u, format!("https://live.bilibili.com/{room}")));
            }
            // 回退：旧版 playUrl 接口 durl 直出完整 FLV 地址（免登录场景下
            // v2 的 url_list 常被服务端置空，旧接口反而稳定可用）。
            let real_room = v["data"]["room_id"]
                .as_i64()
                .map(|x| x.to_string())
                .unwrap_or_else(|| room.to_string());
            let url2 = format!(
                "https://api.live.bilibili.com/room/v1/Room/playUrl?cid={real_room}&qn={qn}&platform=web"
            );
            let resp2 = crate::http::get(&url2, headers.clone())
                .await
                .map_err(|e| format!("B站直播接口请求失败: {e}"))?;
            let v2: Value = serde_json::from_str(&resp2.body).map_err(|_| {
                format!(
                    "B站直播接口响应非 JSON: {}",
                    &resp2.body[..resp2.body.len().min(120)]
                )
            })?;
            if v2.get("code").and_then(|x| x.as_i64()) != Some(0) {
                let msg = v2
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("接口错误");
                return Err(format!("B站直播间 {room} 未开播或获取失败: {msg}"));
            }
            parse_bili_durl(&v2)
                .ok_or_else(|| format!("B站直播间 {room} 未找到可用的 FLV 流（可能未开播）"))
                .map(|u| (u, format!("https://live.bilibili.com/{room}")))
        }
    }
}

/// 同步版 B站流地址解析：解码线程重连时调用（每次拿全新 expires 的地址，
/// 自愈 CDN 临时 404 与地址过期——固定 URL 重连只会反复失败）。
pub fn resolve_stream_sync(room: &str, qn: u32) -> Result<Vec<(String, String)>, String> {
    let url = format!(
        "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={room}&protocol=0,1&format=0,2&codec=0,1&qn={qn}&platform=web&ptype=16"
    );
    let referer_owned = format!("https://live.bilibili.com/{room}");
    let headers = vec![
        ("User-Agent".to_string(), BROWSER_UA.to_string()),
        ("Referer".to_string(), referer_owned.clone()),
    ];
    let body = crate::http::request_sync("GET", &url, &headers, None)
        .map_err(|e| format!("B站直播接口请求失败: {e}"))?
        .body;
    let v: Value = serde_json::from_str(&body)
        .ok()
        .ok_or_else(|| "B站直播接口响应解析失败".to_string())?;
    // 未开播快速失败：playurl 响应 data.live_status != 1 时无流，若继续走
    // durl 解析会拿到占位/失效地址，导致解码线程反复重连直到 15s 超时。
    if let Some(ls) = v["data"]["live_status"].as_i64() {
        if ls != 1 {
            return Err(format!("B站直播间 {room} 未开播"));
        }
    }
    if let Some(u) = parse_bili_flv(&v) {
        return Ok(vec![(u, referer_owned.clone())]);
    }
    let real_room = v["data"]["room_id"]
        .as_i64()
        .map(|x| x.to_string())
        .unwrap_or_else(|| room.to_string());
    let url2 = format!(
        "https://api.live.bilibili.com/room/v1/Room/playUrl?cid={real_room}&qn={qn}&platform=web"
    );
    let headers2 = vec![
        ("User-Agent".to_string(), BROWSER_UA.to_string()),
        ("Referer".to_string(), referer_owned.clone()),
    ];
    let body2 = crate::http::request_sync("GET", &url2, &headers2, None)
        .map_err(|e| format!("B站直播接口请求失败: {e}"))?
        .body;
    let v2: Value = serde_json::from_str(&body2)
        .ok()
        .ok_or_else(|| "B站直播接口响应解析失败".to_string())?;
    if v2.get("code").and_then(|x| x.as_i64()) != Some(0) {
        return Err(format!("B站直播间 {room} 未开播或获取失败"));
    }
    // durl 列表 = 多个 CDN 节点候选：固定用第一个可能撞上慢/故障节点，
    // 全部收下由解码线程轮换连接（慢流/断流自动换节点）。
    let durls = v2["data"]["durl"]
        .as_array()
        .ok_or_else(|| format!("B站直播间 {room} 未找到可用的 FLV 流（可能未开播）"))?;
    let referer = format!("https://live.bilibili.com/{room}");
    let mut out: Vec<(String, String)> = durls
        .iter()
        .filter_map(|u| u["url"].as_str())
        .filter(|s| !s.is_empty())
        .map(|u| (u.to_string(), referer.clone()))
        .collect();
    if out.is_empty() {
        return Err(format!(
            "B站直播间 {room} 未找到可用的 FLV 流（可能未开播）"
        ));
    }
    // 按 host 去重，避免同一节点重复。
    out.dedup_by(|a, b| host_of(&a.0) == host_of(&b.0));
    Ok(out)
}

/// 从 URL 中提取 CDN 主机名（节点去重用）。
fn host_of(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

/// 从 playinfo 响应解析该房间**实际提供**的清晰度档位（降序）。
/// 关键：不同房间档位不同——有的仅原画 [10000]，有的多档 [10000,400,250,150]。
/// 降级链必须据此构建，否则请求不存在的档位会被 B站回退到原画（降级静默失效）。
pub fn parse_accept_qn(v: &Value) -> Vec<u32> {
    let Some(streams) = v["data"]["playurl_info"]["playurl"]["stream"].as_array() else {
        return Vec::new();
    };
    for stream in streams {
        if stream["protocol_name"].as_str() != Some("http_stream") {
            continue;
        }
        let Some(fmts) = stream["format"].as_array() else {
            continue;
        };
        for fmt in fmts {
            if fmt["format_name"].as_str() != Some("flv") {
                continue;
            }
            let Some(codecs) = fmt["codec"].as_array() else {
                continue;
            };
            for codec in codecs {
                if codec["codec_name"].as_str() == Some("avc") {
                    if let Some(arr) = codec["accept_qn"].as_array() {
                        let mut qs: Vec<u32> = arr
                            .iter()
                            .filter_map(|x| x.as_u64().map(|n| n as u32))
                            .collect();
                        qs.sort_unstable_by(|a, b| b.cmp(a));
                        qs.dedup();
                        return qs;
                    }
                }
            }
        }
    }
    Vec::new()
}

/// 从 getRoomPlayInfo 响应提取 http_stream / flv / avc 拉流地址（base_url + host）。
pub fn parse_bili_flv(v: &Value) -> Option<String> {
    let streams = v["data"]["playurl_info"]["playurl"]["stream"].as_array()?;
    for stream in streams {
        if stream["protocol_name"].as_str() != Some("http_stream") {
            continue;
        }
        for fmt in stream["format"].as_array()? {
            if fmt["format_name"].as_str() != Some("flv") {
                continue;
            }
            for codec in fmt["codec"].as_array()? {
                if codec["codec_name"].as_str() == Some("avc") {
                    let base = codec["base_url"].as_str()?;
                    let host = codec["url_list"].as_array()?.first()?.as_str()?;
                    return Some(format!("{host}{base}"));
                }
            }
        }
    }
    None
}

/// 从旧版 playUrl 响应提取 durl 直出的 FLV 地址。
pub fn parse_bili_durl(v: &Value) -> Option<String> {
    v["data"]["durl"]
        .as_array()?
        .iter()
        .find_map(|u| u["url"].as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

// ---------- FLV 解复用 / H.264 辅助（纯函数，可测） ----------

/// AVCC（4 字节长度前缀）→ Annex-B（00 00 00 01 起始码）。
pub fn avcc_to_annexb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let n = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if n == 0 || i + n > data.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[i..i + n]);
        i += n;
    }
    out
}

/// AVCDecoderConfigurationRecord（FLV sequence header 体）→ SPS/PPS Annex-B。
pub fn avcc_config_to_annexb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if data.len() < 7 {
        return out;
    }
    let mut i = 5usize;
    let sps_count = data[i] & 0x1F;
    i += 1;
    let read_nalus = |count: usize, data: &[u8], i: &mut usize, out: &mut Vec<u8>| {
        for _ in 0..count {
            if *i + 2 > data.len() {
                return;
            }
            let n = u16::from_be_bytes([data[*i], data[*i + 1]]) as usize;
            *i += 2;
            if n == 0 || *i + n > data.len() {
                return;
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&data[*i..*i + n]);
            *i += n;
        }
    };
    read_nalus(sps_count as usize, data, &mut i, &mut out);
    if i < data.len() {
        let pps_count = data[i] & 0x1F;
        i += 1;
        read_nalus(pps_count as usize, data, &mut i, &mut out);
    }
    out
}

/// YUV420 平面（BT.601 有限色域）→ RGBA。
/// 保留作为测试基准（生产路径已改按需转换，见 yuv_crop_to_rgb/rgba）。
pub fn yuv420_to_rgba(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    y_stride: usize,
    uv_stride: usize,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let uv_row = row / 2;
        for col in 0..w {
            let yv = y[row * y_stride + col] as i32;
            let uv = u[uv_row * uv_stride + col / 2] as i32 - 128;
            let vv = v[uv_row * uv_stride + col / 2] as i32 - 128;
            let yy = yv - 16;
            let r = (1192 * yy + 1634 * vv) >> 10;
            let g = (1192 * yy - 400 * uv - 833 * vv) >> 10;
            let b = (1192 * yy + 2066 * uv) >> 10;
            out.push(r.clamp(0, 255) as u8);
            out.push(g.clamp(0, 255) as u8);
            out.push(b.clamp(0, 255) as u8);
            out.push(255);
        }
    }
    out
}

/// 紧凑 YUV420 裁剪区 → RGB（3 字节/像素），面积平均降采样到 tw×th。
///
/// **只转换输出像素所需的源区域**（对比早先「全帧转 RGBA 再降采样」），
/// 预览 320 宽时转换量约为全帧的 1/16。Y 取箱内平均，U/V 取对应色度箱平均。
/// `src_w` 为源帧宽（紧凑布局，Y 跨距 = src_w，UV 跨距 = src_w/2）。
#[allow(clippy::too_many_arguments)]
fn yuv_crop_to_rgb(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    src_w: usize,
    src_h: usize,
    crop: (u32, u32, u32, u32),
    tw: usize,
    th: usize,
    step: usize,
) -> Vec<u8> {
    let (cx, cy, cw, ch) = (
        crop.0 as usize,
        crop.1 as usize,
        crop.2 as usize,
        crop.3 as usize,
    );
    let cw = cw.min(src_w.saturating_sub(cx)).max(1);
    let ch = ch.min(src_h.saturating_sub(cy)).max(1);
    let (tw, th) = (tw.max(1), th.max(1));
    let step = step.max(1);
    let cw2 = src_w / 2; // 色度跨距
    let mut out = Vec::with_capacity(tw * th * 3);
    for oy in 0..th {
        let sy0 = cy + oy * ch / th;
        let sy1 = (cy + (oy + 1) * ch / th).max(sy0 + 1);
        let cys0 = sy0 / 2;
        let cys1 = (sy1 / 2).max(cys0 + 1);
        for ox in 0..tw {
            let sx0 = cx + ox * cw / tw;
            let sx1 = (cx + (ox + 1) * cw / tw).max(sx0 + 1);
            // Y 箱步进平均（step=1 为全平均，扫码用；step>1 为采样，预览用）。
            let mut ysum = 0u32;
            let mut yn = 0u32;
            let mut yy = sy0;
            while yy < sy1 {
                let base = yy * src_w;
                let mut xx = sx0;
                while xx < sx1 {
                    ysum += y[base + xx] as u32;
                    yn += 1;
                    xx += step;
                }
                yy += step;
            }
            // 色度箱步进平均（色度是 Y 的一半分辨率，step 相应处理）。
            let cxs0 = sx0 / 2;
            let cxs1 = (sx1 / 2).max(cxs0 + 1);
            let cstep = step.div_ceil(2).max(1);
            let mut us = 0u32;
            let mut vs = 0u32;
            let mut cn = 0u32;
            let mut cyy = cys0;
            while cyy < cys1 {
                let base = cyy * cw2;
                let mut cxx = cxs0;
                while cxx < cxs1 {
                    us += u[base + cxx] as u32;
                    vs += v[base + cxx] as u32;
                    cn += 1;
                    cxx += cstep;
                }
                cyy += cstep;
            }
            let yv = (ysum / yn.max(1)) as i32;
            let uv = (us / cn.max(1)) as i32 - 128;
            let vv = (vs / cn.max(1)) as i32 - 128;
            let yy = yv - 16;
            let r = (1192 * yy + 1634 * vv) >> 10;
            let g = (1192 * yy - 400 * uv - 833 * vv) >> 10;
            let b = (1192 * yy + 2066 * uv) >> 10;
            out.push(r.clamp(0, 255) as u8);
            out.push(g.clamp(0, 255) as u8);
            out.push(b.clamp(0, 255) as u8);
        }
    }
    out
}

/// 同 yuv_crop_to_rgb，但输出 RGBA（预览协议用，多一字节 255）。
#[allow(clippy::too_many_arguments)]
fn yuv_crop_to_rgba(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    src_w: usize,
    src_h: usize,
    crop: (u32, u32, u32, u32),
    tw: usize,
    th: usize,
    step: usize,
) -> Vec<u8> {
    let rgb = yuv_crop_to_rgb(y, u, v, src_w, src_h, crop, tw, th, step);
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.as_chunks::<3>().0 {
        out.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    out
}

// ---------- 会话与帧总线 ----------

/// 解码产出的一帧（紧凑 YUV420 / I420）。
///
/// **为什么存 YUV 而不是 RGBA**：解码器原生输出即 YUV420（1.5 字节/像素），
/// 早先每帧全帧转 RGBA（4 字节/像素）纯属浪费——720p 每帧要转换 92 万像素，
/// 30fps 下每秒 2760 万像素；而预览只输出 ~320 宽、扫码只取 ROI。
/// 现改存紧凑 YUV（内存与拷贝量降 62%），把 YUV→RGB 转换**推迟到消费端、
/// 且只转换输出像素所需的源区域**（转换量降一个数量级）。
struct Frame {
    seq: u64,
    /// FLV 媒体时间戳（ms，流自身时钟）。输出侧追帧与诊断用。
    ts: u32,
    w: u32,
    h: u32,
    /// Y 平面，紧凑布局（行跨距 = w）。
    y: Vec<u8>,
    /// U 平面（w/2 × h/2，行跨距 = w/2）。
    u: Vec<u8>,
    /// V 平面（w/2 × h/2，行跨距 = w/2）。
    v: Vec<u8>,
}

/// 帧总线：小型回放缓冲（FIFO）+ 独立的最新帧。
///
/// **两条消费路径必须解耦**：
/// - 显示路径（crop_frame，前端 30fps 轮询）破坏性弹出队列最旧帧做节拍控制；
/// - 扫码路径（take_scan_frame，约 400ms 一次）只关心「当前最新画面」。
///
/// 早期设计让扫码也读同一队列的 back()：低帧率流（720p ~10fps）下显示弹出(30fps)
/// 远快于到达(10fps)，队列常被榨空 → 扫码永远取不到帧（实测 0%）。现在解码线程
/// 同时维护 latest（Arc 共享，零拷贝），扫码只读它，与显示解耦。
struct Frames {
    queue: std::collections::VecDeque<Arc<Frame>>,
    /// 最新一帧（解码线程每次产出即更新；扫码专用，不受显示弹出影响）。
    latest: Option<Arc<Frame>>,
    /// 上次扫码输出的 RGB 缓存。无新帧时复用，保证扫码供帧率 100%——
    /// 低帧率流（如 ~2fps）不会因某轮 400ms 窗口内无新帧而「停一拍」
    /// （实测 100x178 流会丢 15%~23% 的采样轮次）。
    scan_cache: Option<ScanCache>,
    /// 单调递增的帧序号。**不能**用「队列尾 seq + 1」——队列空时那会导致
    /// 新帧 seq 恒为 1，使 LAST_SENT 去重把不同帧误判为同一帧（预览卡成 1 帧）。
    next_seq: u64,
}

/// 从解码器输出的 Y/U/V 平面（可能带行跨距）拷贝为紧凑布局的 Frame。
#[allow(clippy::too_many_arguments)]
fn frame_from_yuv(
    seq: u64,
    ts: u32,
    w: u32,
    h: u32,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    ys: usize,
    us: usize,
) -> Frame {
    let (w, h) = (w as usize, h as usize);
    let cw = w / 2;
    let ch = h / 2;
    let mut yc = Vec::with_capacity(w * h);
    for row in 0..h {
        let base = row * ys;
        yc.extend_from_slice(&y[base..base + w]);
    }
    let mut uc = Vec::with_capacity(cw * ch);
    let mut vc = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        let base = row * us;
        uc.extend_from_slice(&u[base..base + cw]);
        vc.extend_from_slice(&v[base..base + cw]);
    }
    Frame {
        seq,
        ts,
        w: w as u32,
        h: h as u32,
        y: yc,
        u: uc,
        v: vc,
    }
}

/// 队列硬上限（≈200ms 显示缓冲；防极端突发撑爆内存，超出丢弃最旧帧）。
const FRAME_BUFFER_MAX: usize = 6;

/// 扫码 RGB 缓存：键为 (帧序号, 裁剪区)，视口不变且无新帧时直接复用。
struct ScanCache {
    seq: u64,
    crop: Option<(u32, u32, u32, u32)>,
    rgb: Vec<u8>,
    w: u32,
    h: u32,
}

/// 上次发送的预览帧标识：(seq, 裁剪区, 输出宽)。相同则返回短响应。
type SentKey = (u64, (u32, u32, u32, u32), u32);
static LAST_SENT: Mutex<Option<SentKey>> = Mutex::new(None);
/// 最近一次预览请求的视口（流坐标系）；扫码只分析该区域（与屏幕扫码
/// 「可视视口即扫描范围」语义一致，也大幅降低解码成本）。
static LAST_VIEW: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

struct LiveSession {
    stop: Arc<AtomicBool>,
    frames: Arc<Mutex<Frames>>,
    room: String,
}

static LIVE: Mutex<Option<LiveSession>> = Mutex::new(None);

fn lock_live() -> std::sync::MutexGuard<'static, Option<LiveSession>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 启动直播会话（解析流地址 + 启动解码线程，阻塞至拿到首帧或超时）。
pub async fn start(room: String) -> Result<(u32, u32), String> {
    stop();
    *LAST_SENT.lock().unwrap_or_else(PoisonError::into_inner) = None;
    // 初始校验：房间可解析、可开播（失败快速反馈给前端）；
    // 实际拉流地址由解码线程每次连接/重连时重新获取（新 expires，自愈临时故障）。
    resolve_stream(&room, 10000).await?;
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(Mutex::new(Frames {
        queue: std::collections::VecDeque::new(),
        latest: None,
        scan_cache: None,
        next_seq: 0,
    }));
    let fail: FailFlag = Arc::new(Mutex::new(None));
    let (room2, stop2, frames2, fail2) = (room.clone(), stop.clone(), frames.clone(), fail.clone());
    std::thread::Builder::new()
        .name("live-decode".into())
        .spawn(move || decode_loop(room2, stop2, frames2, fail2))
        .map_err(|e| format!("解码线程启动失败: {e}"))?;
    // 等首帧（流建立 + 关键帧到达，通常 1~4 秒）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        {
            let f = frames.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(back) = f.queue.back() {
                if back.w > 0 {
                    let (w, h) = (back.w, back.h);
                    drop(f);
                    *lock_live() = Some(LiveSession {
                        stop,
                        frames,
                        room: room.clone(),
                    });
                    return Ok((w, h));
                }
            }
        }
        if stop.load(Ordering::SeqCst) {
            return Err("直播会话已被取消".to_string());
        }
        // 解码线程回报致命失败：立即返回明确错误，不必干等 15 秒超时。
        if let Some(reason) = fail.lock().unwrap_or_else(PoisonError::into_inner).take() {
            stop.store(true, Ordering::SeqCst);
            return Err(reason);
        }
        if tokio::time::Instant::now() >= deadline {
            stop.store(true, Ordering::SeqCst);
            return Err(format!("B站直播间 {room} 连接超时（15 秒内未取到画面）"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 停止当前直播会话（断流 + 释放）。
pub fn stop() {
    if let Some(s) = lock_live().take() {
        s.stop.store(true, Ordering::SeqCst);
        log::info!("直播会话已断开（B站 {}）", s.room);
    }
}

/// 是否存在活跃会话。
pub fn active() -> bool {
    lock_live().is_some()
}

/// 诊断：最新解码帧的 (seq, 媒体时间戳, 队列深度)。探针观测延迟漂移用。
pub fn diag() -> Option<(u64, u32, usize)> {
    let live = lock_live();
    let s = live.as_ref()?;
    let f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
    let b = f.latest.as_ref()?;
    Some((b.seq, b.ts, f.queue.len()))
}

/// 扫码消费者：自上次消费以来有新帧时返回最新帧的**当前显示视口**区域
/// （2×2 平均半尺寸 RGB）。视口未知时用全帧。
pub fn take_scan_frame() -> Option<(Vec<u8>, u32, u32)> {
    let live = lock_live();
    let s = live.as_ref()?;
    let view = *LAST_VIEW.lock().unwrap_or_else(PoisonError::into_inner);
    let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
    // 读独立的 latest（不是显示队列）——显示弹出不影响扫码取帧。
    let frame = f.latest.clone()?;
    if frame.seq == 0 {
        return None;
    }
    let (fw, fh) = (frame.w, frame.h);
    let crop = view.and_then(|(x, y, w, h)| {
        if w <= 0 || h <= 0 {
            return None;
        }
        let x0 = x.clamp(0, w - 1);
        let y0 = y.clamp(0, h - 1);
        let cw0 = (w.min(w - x0)).max(2) as u32;
        let ch0 = (h.min(h - y0)).max(2) as u32;
        Some((x0 as u32, y0 as u32, cw0, ch0))
    });
    // 无新帧且缓存命中（视口未变）：复用上次 RGB，保证供帧率 100%。
    if let Some(c) = &f.scan_cache {
        if c.seq == frame.seq && c.crop == crop {
            return Some((c.rgb.clone(), c.w, c.h));
        }
    }
    // 输出半尺寸（2×2 面积平均）——二维码半尺寸足够，成本约 1/4。
    // 扫码要求还原度：step=1 全平均（输出半尺寸，等于 2×2 面积平均）。
    let (rgb, ow, oh) = match crop {
        Some(c) => {
            let (hw, hh) = ((c.2 / 2).max(1), (c.3 / 2).max(1));
            (
                yuv_crop_to_rgb(
                    &frame.y,
                    &frame.u,
                    &frame.v,
                    fw as usize,
                    fh as usize,
                    c,
                    hw as usize,
                    hh as usize,
                    1,
                ),
                hw,
                hh,
            )
        }
        None => {
            let (hw, hh) = ((fw / 2).max(1), (fh / 2).max(1));
            (
                yuv_crop_to_rgb(
                    &frame.y,
                    &frame.u,
                    &frame.v,
                    fw as usize,
                    fh as usize,
                    (0, 0, fw, fh),
                    hw as usize,
                    hh as usize,
                    1,
                ),
                hw,
                hh,
            )
        }
    };
    f.scan_cache = Some(ScanCache {
        seq: frame.seq,
        crop,
        rgb: rgb.clone(),
        w: ow,
        h: oh,
    });
    Some((rgb, ow, oh))
}

/// 预览消费者：按视口裁剪 + 面积降采样，组装 24 字节头 + RGBA 负载。
/// 头部布局与屏幕预览一致（mon = 流原始尺寸），无会话/无帧返回 None。
/// 预览帧输出语义。
/// - `NoSession`：会话不存在（未启动 / 已结束被移除）→ 前端自动切回屏幕；
/// - `Empty`：会话存活但缓冲欠载（流帧率低于显示节拍属正常现象）或帧未变
///   → 前端沿用上一帧；
/// - `Frame`：完整 24 字节头 + RGBA。
pub enum FrameOut {
    NoSession,
    Empty,
    Frame(Vec<u8>),
}

pub fn crop_frame(x: i32, y: i32, w: i32, h: i32, max_width: u32) -> FrameOut {
    *LAST_VIEW.lock().unwrap_or_else(PoisonError::into_inner) = if w > 0 && h > 0 {
        Some((x, y, w, h))
    } else {
        None
    };
    let live = lock_live();
    let s = match live.as_ref() {
        Some(s) => s,
        None => return FrameOut::NoSession,
    };
    let mut f = s.frames.lock().unwrap_or_else(PoisonError::into_inner);
    if f.queue.is_empty() {
        return FrameOut::Empty;
    }
    // 弹出节拍 = 显示节拍（前端 30fps 轮询）：每轮询至多弹出一个最旧帧，
    // 队列空时复用队首帧。显示间隔完全由轮询时钟决定（均匀 30fps），到达抖动
    // 被缓冲吸收——「到达即弹出」会把抖动直接透传，缓冲再深也无济于事。
    let popped = f.queue.pop_front();
    let f = match &popped {
        Some(fr) => fr,
        None => f.queue.front().unwrap(),
    };
    if f.w == 0 || f.h == 0 {
        return FrameOut::Empty;
    }
    let f = f.clone();
    let (fw, fh) = (f.w as i32, f.h as i32);
    let (cx, cy, cw, ch) = if w > 0 && h > 0 {
        let x0 = x.clamp(0, fw - 1);
        let y0 = y.clamp(0, fh - 1);
        let cw0 = (w.min(fw - x0)).max(1) as u32;
        let ch0 = (h.min(fh - y0)).max(1) as u32;
        (x0 as u32, y0 as u32, cw0, ch0)
    } else {
        (0, 0, f.w, f.h)
    };
    // 输出宽度不超过裁剪区实际像素数（禁上采样，避免黑网格）。
    let tw = max_width.min(cw).clamp(1, 4096);
    let th = (((ch as u64) * tw as u64) / cw as u64).max(1) as u32;
    // 帧未变（seq 相同）且视口/宽度未变：24 字节短响应，前端沿用上一帧
    // （复用屏幕预览协议），消除重复帧的降采样与 IPC。
    let key = (f.seq, (cx, cy, cw, ch), tw);
    {
        let mut last = LAST_SENT.lock().unwrap_or_else(PoisonError::into_inner);
        if *last == Some(key) {
            return FrameOut::Empty;
        }
        *last = Some(key);
    }
    // 预览允许采样：步长与旧 downsample_rgba 一致（cv/tw 后再折半），
    // 保证预览转换量约为全帧的 1/4~1/16。
    let step = (cw.div_ceil(tw).max(1).div_ceil(2).max(1)) as usize;
    let rgba = yuv_crop_to_rgba(
        &f.y,
        &f.u,
        &f.v,
        f.w as usize,
        f.h as usize,
        (cx, cy, cw, ch),
        tw as usize,
        th as usize,
        step,
    );
    let mut out = Vec::with_capacity(24 + rgba.len());
    out.extend_from_slice(&tw.to_le_bytes());
    out.extend_from_slice(&th.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&f.w.to_le_bytes());
    out.extend_from_slice(&f.h.to_le_bytes());
    out.extend_from_slice(&rgba);
    FrameOut::Frame(out)
}

// ---------- 解码线程 ----------

/// 构建房间的清晰度降级链：仅保留房间实际提供、且 ≤10000 的档位（不主动上 4K/杜比），
/// 降序排列。房间仅原画时链长为 1（不降级，避免无效重连）。
fn build_qn_chain(accept: &[u32]) -> Vec<u32> {
    let mut chain: Vec<u32> = accept.iter().copied().filter(|q| *q <= 10000).collect();
    chain.sort_unstable_by(|a, b| b.cmp(a));
    chain.dedup();
    if chain.is_empty() {
        // accept_qn 解析失败/异常：退化为「仅原画」，不降级。
        chain.push(10000);
    }
    chain
}

/// 获取房间实际提供的清晰度降级链（一次 API 查询）。失败退化为 [10000]。
fn fetch_qn_chain(room: &str) -> Vec<u32> {
    let url = format!(
        "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={room}&protocol=0,1&format=0,2&codec=0,1&qn=10000&platform=web&ptype=16"
    );
    let referer = format!("https://live.bilibili.com/{room}");
    let headers = vec![
        ("User-Agent".to_string(), BROWSER_UA.to_string()),
        ("Referer".to_string(), referer),
    ];
    let v: Option<Value> = crate::http::request_sync("GET", &url, &headers, None)
        .ok()
        .and_then(|r| serde_json::from_str(&r.body).ok());
    match v {
        Some(v) => build_qn_chain(&parse_accept_qn(&v)),
        None => vec![10000],
    }
}
/// 解码持续失败的容忍次数：超过则降级清晰度重试。
const DECODE_FAIL_LIMIT: u32 = 40;

/// 解码器配置：NoFlush（保留参考帧链）。
///
/// **不可启用多线程**：openh264 的 num_threads 在 Windows 上实测必崩
/// （STATUS_ACCESS_VIOLATION / 0xc0000005），库方文档亦明确警告 "will probably
/// segfault"。1080p 单线程解码跟不上码率的房间，改用「追帧维持低延迟 + 自动降清晰度」
/// 处理（见 decode_session / decode_loop）。
fn decode_config() -> openh264::decoder::DecoderConfig {
    openh264::decoder::DecoderConfig::new().flush_after_decode(openh264::decoder::Flush::NoFlush)
}

/// 直播追帧阈值（毫秒）：输出帧媒体时间戳落后「应有值」超过此值进入快进态。
/// 权衡：过小会因解码偶发波动频繁快进（顿挫），过大则延迟偏高；
/// 1.2s 远低于浏览器端 2~4s，且给解码留缓冲余量。
const CATCHUP_LAG_MS: i64 = 1200;
/// 快进追不动的严重积压阈值：落后超过 3×CATCHUP 时跳到关键帧硬追
/// （解码吞吐不足以支撑快进时的兜底）。
const SEVERE_LAG_MS: i64 = 3 * CATCHUP_LAG_MS;
/// 快进视为追上的滞后余量。
const CATCHUP_DONE_MS: i64 = 300;

/// 持续追帧上限：连续快速追帧（间隔 < 4s）达到此次数，判为解码吞吐持续不足，
/// 降级清晰度换低码率流。仅「持续跟不上」才降级，偶发追帧不触发。
const CATCHUP_LIMIT: u32 = 4;

/// 解码线程向 start() 回报的致命失败原因（未开播 / 解码不支持 / 连续重连失败）。
/// start() 轮询它以便快速返回明确错误，而非干等 15 秒首帧超时。
type FailFlag = Arc<Mutex<Option<String>>>;

/// 当前输出帧相对墙钟锚点的滞后量（ms）。
fn out_lag_ms(anchor: Option<(u32, std::time::Instant)>, media_ts: u32) -> i64 {
    match anchor {
        Some((a_ts, a_at)) => a_ts as i64 + a_at.elapsed().as_millis() as i64 - media_ts as i64,
        None => 0,
    }
}

/// 记录致命失败并让解码线程返回。
fn set_fail(fail: &FailFlag, reason: &str) {
    *fail.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason.to_string());
}

fn decode_loop(room: String, stop: Arc<AtomicBool>, frames: Arc<Mutex<Frames>>, fail: FailFlag) {
    // 按房间实际提供的档位构建降级链（不同房间档位不同）。
    let qn_chain = fetch_qn_chain(&room);
    log::info!("房间 {room} 清晰度档位链: {qn_chain:?}");
    let mut fails = 0u32;
    let mut qn_idx = 0usize;
    // 候选节点池（多 CDN 节点轮换：固定一个可能撞上慢/故障节点）。
    let mut candidates: Vec<(String, String)> = Vec::new();
    let mut cand_used = 0usize;
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if candidates.is_empty() {
            candidates = match resolve_stream_sync(&room, qn_chain[qn_idx]) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("解析流地址失败: {e}");
                    fails += 1;
                    if fails > 4 {
                        log::error!("直播流连续解析失败，会话关闭");
                        set_fail(&fail, &format!("B站直播间 {room} 流地址解析失败：{e}"));
                        remove_session_if(&frames);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(400));
                    continue;
                }
            };
            cand_used = 0;
        }
        if cand_used >= candidates.len() {
            // 本轮候选全部失败：重新解析（新 expires 自愈 + 轮换节点）。
            candidates.clear();
            continue;
        }
        let (url, referer) = candidates[cand_used].clone();
        cand_used += 1;
        log::info!("连接直播流节点 {cand_used}/{} …", candidates.len());
        let allow_degrade = qn_idx + 1 < qn_chain.len();
        match decode_session(&url, &referer, &stop, &frames, allow_degrade) {
            Ok(()) => return, // 收到停止信号
            // 解码器持续失败（编码不兼容/解码跟不上）：降级清晰度换低码率流重试。
            Err(e) if e.starts_with("H.264 解码持续失败") => {
                if qn_idx + 1 < qn_chain.len() {
                    qn_idx += 1;
                    fails = 0;
                    candidates.clear();
                    log::info!("清晰度降级到 qn={} 重试", qn_chain[qn_idx]);
                    continue;
                }
                log::error!("最低清晰度仍无法解码，会话关闭");
                set_fail(
                    &fail,
                    &format!("B站直播间 {room} 编码格式不受支持（openh264 解码失败）"),
                );
                remove_session_if(&frames);
                return;
            }
            // 解码吞吐持续不足（追帧过频）：降级清晰度换低码率流。
            // （decode_session 只在还能降级时返回此错误，故此处 qn_idx 必可前进。）
            Err(e) if e.starts_with("直播追帧过于频繁") => {
                qn_idx += 1;
                fails = 0;
                candidates.clear();
                log::info!("解码吞吐不足，清晰度降级到 qn={} 重试", qn_chain[qn_idx]);
                continue;
            }
            Err(e) => log::warn!("直播流断开: {e}"),
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        fails += 1;
        // 下播时 CDN 会快速关闭连接，重连也立即失败——快速放弃并移除会话。
        if fails > 8 {
            log::error!("直播流连续重连失败，会话关闭");
            set_fail(
                &fail,
                &format!("B站直播间 {room} 连接失败（CDN 节点均不可用）"),
            );
            remove_session_if(&frames);
            return;
        }
        log::info!("直播流重连（第 {fails} 次，尝试下一节点）…");
        std::thread::sleep(Duration::from_millis(400));
    }
}

/// 仅当 LIVE 中仍是本线程对应的会话时移除（防止误删替换后的新会话）。
fn remove_session_if(frames: &Arc<Mutex<Frames>>) {
    let mut live = lock_live();
    if let Some(s) = live.as_ref() {
        if Arc::ptr_eq(&s.frames, frames) {
            live.take();
            log::info!("直播会话已关闭（流结束/重连失败），回退屏幕预览与扫码");
        }
    }
}

/// 慢流判定读超时：CDN 节点 3 秒不推数据 → read 超时 → 换节点。
const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// 手写 HTTP/1.1 流式读取（http-flv 用）。
///
/// 为什么不用 ureq：实测 ureq 2.12 的 `into_reader()` 对 B站 http-flv 的
/// chunked 长连接会卡死（10 秒仅读到 2 个 tag，curl 同流 60fps 正常）。
/// 这里手写 GET + chunked 解码，读超时兼作慢流检测。
pub struct HttpFlvReader {
    stream: Box<dyn Read>,
    /// 响应是否用 chunked 传输编码。
    /// **关键**：B站不同 CDN 服务器行为不一——JSP3 返回 chunked，Tengine 返回
    /// 裸 FLV（Connection: close 直出）。必须按响应头区分，否则裸流会被误当
    /// chunk 头解析（"FLV\x01" 被当 hex 大小行）而读不出任何数据。
    chunked: bool,
    /// 当前 chunk 剩余字节（仅 chunked 模式使用）。
    chunk_left: usize,
    done: bool,
}

impl Read for HttpFlvReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        // 裸 FLV 模式：直接透传底层流（Connection: close 直出，读到 EOF 为止）。
        if !self.chunked {
            return self.stream.read(buf);
        }
        if self.chunk_left == 0 {
            // 读 chunk 大小行（hex + CRLF）。
            let mut size_line = Vec::with_capacity(16);
            loop {
                let mut b = [0u8; 1];
                if self.stream.read(&mut b)? == 0 {
                    return Ok(0);
                }
                size_line.push(b[0]);
                if size_line.ends_with(b"\r\n") {
                    break;
                }
                if size_line.len() > 32 {
                    return Err(std::io::Error::other("chunk 大小行过长"));
                }
            }
            let hex = String::from_utf8_lossy(&size_line[..size_line.len() - 2]);
            let size = usize::from_str_radix(hex.trim(), 16).map_err(std::io::Error::other)?;
            if size == 0 {
                // 终止 chunk：吃掉结尾 CRLF。
                let mut crlf = [0u8; 2];
                let _ = self.stream.read(&mut crlf)?;
                self.done = true;
                return Ok(0);
            }
            self.chunk_left = size;
        }
        let n = buf.len().min(self.chunk_left);
        let got = self.stream.read(&mut buf[..n])?;
        self.chunk_left -= got;
        if self.chunk_left == 0 {
            // chunk 数据后的 CRLF。
            let mut crlf = [0u8; 2];
            let _ = self.stream.read(&mut crlf)?;
        }
        Ok(got)
    }
}

impl HttpFlvReader {
    /// 连接 https://... 并发起 GET，返回已就绪的流式读取器（响应头已消费）。
    ///
    /// **跟随重定向**：B站 CDN 常返回 302 跳转到实际流节点（curl -L 同行为）。
    /// 早期实现把 302 当"节点不可用"直接失败，导致永远读不到流。
    pub fn connect(url: &str, referer: &str) -> Result<Self, String> {
        let mut current = url.to_string();
        // 302 跳转链一般 1~2 跳，给足 6 次冗余。
        for _ in 0..6 {
            let (https, host, port, path) =
                crate::http::split_url(&current).map_err(|e| format!("流地址解析失败: {e}"))?;
            if !https {
                return Err("明文 http 已禁用".to_string());
            }
            // rustls：纯 Rust TLS。Windows schannel（native-tls）对该 CDN 长连接
            // 会出现读乱码/EOF（同请求 curl / OpenSSL / rustls 均正常拿到 FLV）。
            // 读超时兼作慢流检测（3 秒无数据换节点）。
            let mut tls = crate::http::tls_connect(&host, port, STREAM_READ_TIMEOUT)?;
            write!(
                tls,
                "GET {path} HTTP/1.1
Host: {host}
User-Agent: {BROWSER_UA}
Referer: {referer}
Accept: */*
Connection: keep-alive

"
            )
            .map_err(|e| format!("发送请求失败: {e}"))?;
            // rustls Writer 有内部缓冲：必须 flush 请求才会真正发出。
            tls.flush()
                .map_err(|e| format!("发送请求失败(flush): {e}"))?;
            // 读响应头直到空行（缓冲式读取：一次读一块，避免逐字节 syscall 与
            // 单字节读在慢首包下的脆弱性）。
            let mut headers = Vec::with_capacity(1024);
            let mut buf = [0u8; 4096];
            loop {
                let n = tls
                    .read(&mut buf)
                    .map_err(|e| format!("读取响应头失败: {e}"))?;
                if n == 0 {
                    return Err("响应中断".to_string());
                }
                headers.extend_from_slice(&buf[..n]);
                if headers.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if headers.len() > 16 * 1024 {
                    return Err("响应头过大".to_string());
                }
            }
            // 只保留响应头部分（缓冲可能多读了首块 body 的前几个字节）。
            let hdr_end = headers
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4)
                .unwrap_or(headers.len());
            let leftover: Vec<u8> = headers[hdr_end..].to_vec();
            headers.truncate(hdr_end);
            // 状态行解析：HTTP/1.x <code>。
            let status = headers
                .split(|&b| b == b' ')
                .nth(1)
                .and_then(|s| std::str::from_utf8(s).ok())
                .and_then(|s| s.parse::<u16>().ok())
                .ok_or_else(|| {
                    format!(
                        "无法解析 HTTP 状态行: {}",
                        String::from_utf8_lossy(&headers[..headers.len().min(80)])
                    )
                })?;
            // 3xx 重定向：读 Location 并跟随（B站 CDN 常态）。
            if (300..400).contains(&status) {
                let loc = headers
                    .split(|&b| b == b'\n')
                    .find_map(|line| {
                        let line = line.strip_suffix(b"\r").unwrap_or(line);
                        let (k, v) = line.split_at(line.iter().position(|&b| b == b':')?);
                        if k.eq_ignore_ascii_case(b"location") {
                            Some(v[1..].trim_ascii())
                        } else {
                            None
                        }
                    })
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .ok_or_else(|| format!("HTTP {status} 重定向缺少 Location 头"))?;
                current = if loc.starts_with("http://") || loc.starts_with("https://") {
                    loc.to_string()
                } else {
                    format!("https://{host}{loc}")
                };
                continue;
            }
            if status != 200 {
                return Err(format!(
                    "直播流 HTTP {status}: {}",
                    String::from_utf8_lossy(&headers[..headers.len().min(120)])
                ));
            }
            // 依响应头判定传输编码：有 Transfer-Encoding: chunked 才走 chunked 解析。
            let chunked = headers
                .split(|&b| b == b'\n')
                .filter_map(|line| {
                    let line = line.strip_suffix(b"\r").unwrap_or(line);
                    let (k, v) = line.split_at(line.iter().position(|&b| b == b':')?);
                    Some((k, v[1..].trim_ascii()))
                })
                .any(|(k, v)| {
                    k.eq_ignore_ascii_case(b"transfer-encoding")
                        && v.eq_ignore_ascii_case(b"chunked")
                });
            log::info!(
                "直播流已连接（传输编码: {}）",
                if chunked { "chunked" } else { "raw" }
            );
            // 响应头读取时多读的 body 前置字节拼回流**最前端**（流级别前缀），
            // 让它们照常进入后续解析——直接透传会破坏 chunk 头/FLV 边界。
            let stream = std::io::Cursor::new(leftover).chain(tls);
            return Ok(HttpFlvReader {
                stream: Box::new(stream),
                chunked,
                chunk_left: 0,
                done: false,
            });
        }
        Err("重定向次数过多（>6）".to_string())
    }
}

/// 从流中精确读取 n 字节（FLV tag 头/体用）；EOF/超时返回「流已结束」。
fn read_exact(reader: &mut impl Read, buf: &mut Vec<u8>, n: usize) -> Result<(), String> {
    buf.resize(n, 0);
    reader
        .read_exact(&mut buf[..n])
        .map_err(|_| "流已结束".to_string())
}

fn decode_session(
    url: &str,
    referer: &str,
    stop: &AtomicBool,
    frames: &Arc<Mutex<Frames>>,
    allow_degrade: bool,
) -> Result<(), String> {
    let mut reader = HttpFlvReader::connect(url, referer)?;

    // FLV 头（9 字节）+ PreviousTagSize0（4 字节）。
    let mut hdr = Vec::new();
    read_exact(&mut reader, &mut hdr, 13)?;
    if &hdr[0..3] != b"FLV" {
        return Err("响应不是 FLV 流".to_string());
    }

    // 关键：NoFlush 模式。默认每帧 decode 后强制 flush 会把参考帧链冲掉，
    // P 帧全部因无参考失败（实测 1080p60 流仅解出 110/1261 帧）；NoFlush 保留
    // 参考链，配合每 FLUSH_FRAME_INTERVAL 帧 flush_remaining 批量取帧（实测 1259/1261）。
    let mut decoder = openh264::decoder::Decoder::with_api_config(
        openh264::OpenH264API::from_source(),
        decode_config(),
    )
    .map_err(|e| format!("解码器初始化失败: {e}"))?;
    let mut dec_fails = 0u32;
    let (mut out_ok, mut out_none, mut out_err) = (0u32, 0u32, 0u32);

    let mut tag = Vec::new();
    // 慢流检测：某节点推流过慢（CDN 故障/限流）时返回错误，上层换节点。
    let mut last_data_at = std::time::Instant::now();
    // 统计窗口：6 秒内视频帧不足 30（≈5fps）判节点劣化——视频 tag 稀疏
    // 而音频不断时 3 秒检测会被绕过，窗口法稳定覆盖。
    let mut window_frames = 0u32;
    let mut window_start = std::time::Instant::now();
    // ---- 直播追帧状态 ----
    // 锚点：(首个视频 tag 的媒体时间戳, 其到达墙钟)。直播媒体时间戳≈推流墙钟，
    // 据此推算「此刻应有的媒体时间戳」，与实际 tag 时间戳之差即落后量。
    let mut anchor: Option<(u32, std::time::Instant)> = None;
    // 是否处于追帧丢弃态（排空积压，等下一个关键帧恢复）。
    let mut skipping = false;
    // 追帧开始时刻（用于超时保护：异常流长时间无关键帧时强制重连，避免假死）。
    let mut skip_since: Option<std::time::Instant> = None;
    // 快进态：Some((倍率, 起始时刻))。解码持续进行（参考链完整、无跳变），
    // 仅按 1/倍率 入队——画面平滑快进追上直播边缘。倍率 2 起，4 秒未追上升 4。
    let mut ff: Option<(u32, std::time::Instant)> = None;
    let mut ff_tick: u32 = 0;
    // 本会话累计追帧次数。健康流几乎不需要追帧；某会话追帧达 CATCHUP_LIMIT 即判
    // 解码吞吐不足 → 降级清晰度（新会话计数从 0 起，自然沿 qn 链逐级下探）。
    let mut catchup_count = 0u32;
    // 缓存的 SPS/PPS（追帧重建解码器后需重喂）。
    let mut param_sets: Vec<u8> = Vec::new();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        // 连接后 3 秒无任何视频数据：节点劣化，切换节点重连。
        if last_data_at.elapsed() > Duration::from_secs(3) {
            return Err("流传输过慢（CDN 节点劣化），切换节点重连".to_string());
        }
        // 6 秒窗口帧数不足：节点只推音频+零星视频的劣化形态。
        if window_start.elapsed() >= Duration::from_secs(6) {
            if window_frames < 30 {
                return Err("流传输过慢（6 秒不足 30 帧，CDN 节点劣化），切换节点重连".to_string());
            }
            window_start = std::time::Instant::now();
            window_frames = 0;
        }
        // Tag 头 11 字节：type(1) data_size(3) timestamp(3+1) stream_id(3)。
        read_exact(&mut reader, &mut tag, 11)?;
        let tag_type = tag[0];
        let data_size = ((tag[1] as usize) << 16) | ((tag[2] as usize) << 8) | tag[3] as usize;
        // FLV 时间戳（媒体时钟，ms）：低 24 位在 [4..7]，高 8 位在 [7]。
        // 必须在读 body 前提取（read_exact 会覆盖 tag 缓冲）。
        let media_ts = ((tag[7] as u32) << 24)
            | ((tag[4] as u32) << 16)
            | ((tag[5] as u32) << 8)
            | tag[6] as u32;
        if data_size > 8 * 1024 * 1024 {
            return Err("FLV Tag 超过大小上限".to_string());
        }
        read_exact(&mut reader, &mut tag, data_size + 4)?; // 体 + PreviousTagSize
        if tag_type != 9 || data_size < 5 {
            // 音频等非视频 tag 不断不代表流正常——慢流节拍只看视频数据。
            continue;
        }
        last_data_at = std::time::Instant::now();
        window_frames += 1;
        let body = &tag[..data_size];
        let first = body[0];
        let frame_type = first >> 4;
        if first & 0x0F != 7 {
            continue; // 非 AVC
        }

        // ---- 直播追帧 ----
        // 落后量 = 应有媒体时间戳（锚点 + 已过墙钟）− 当前 tag 媒体时间戳。
        // 超阈值说明解码吞吐跟不上码率、socket 缓冲持续积压（延迟单调增长）。
        let now = std::time::Instant::now();
        match anchor {
            None => anchor = Some((media_ts, now)),
            Some((a_ts, a_at)) => {
                let expected = a_ts as i64 + a_at.elapsed().as_millis() as i64;
                let lag = expected - media_ts as i64;
                if !skipping && lag > CATCHUP_LAG_MS {
                    skipping = true;
                    skip_since = Some(now);
                    catchup_count += 1;
                    log::info!(
                        "直播追帧：落后 {lag}ms（本会话第 {catchup_count} 次），丢弃至下一关键帧"
                    );
                }
            }
        }
        if skipping {
            // 超时保护：追帧态持续过久仍无关键帧（流异常/丢 SPS）→ 重连，避免假死。
            if skip_since.is_some_and(|t| t.elapsed() > Duration::from_secs(8)) {
                return Err("直播追帧超时（长时间无关键帧），重连".to_string());
            }
            // 只读不解码，快速排空 socket 缓冲；遇关键帧（IDR）重建解码器恢复。
            if frame_type == 1 && body[1] == 1 && !param_sets.is_empty() {
                decoder = openh264::decoder::Decoder::with_api_config(
                    openh264::OpenH264API::from_source(),
                    decode_config(),
                )
                .map_err(|e| format!("解码器重建失败: {e}"))?;
                let _ = decoder.decode(&param_sets);
                skipping = false;
                skip_since = None;
                anchor = Some((media_ts, std::time::Instant::now()));
                log::info!("直播追帧：已在关键帧恢复（ts={media_ts}ms）");
                // 本会话追帧达上限：解码吞吐跟不上码率。仅在还能降级时返回错误；
                // 已是最低清晰度则继续运行（追帧本身已能维持延迟，重连反而中断画面）。
                if catchup_count >= CATCHUP_LIMIT && allow_degrade {
                    return Err("直播追帧过于频繁（解码吞吐不足），降级清晰度重试".to_string());
                }
            } else {
                continue;
            }
        }

        // 非参考帧（frame_type=3，disposable inter）不影响后续解码的参考链，
        // 直接跳过解码：若 60fps 流用它做时间分层，可近乎减半解码负载（1080p60
        // 单线程跟不上时尤为关键）。扫码/预览少这些帧无碍。
        if frame_type == 3 && body[1] == 1 {
            continue;
        }

        match body[1] {
            0 => {
                // AVC sequence header：AVCDecoderConfigurationRecord → SPS/PPS。
                param_sets = avcc_config_to_annexb(&body[5..]);
                log::info!(
                    "AVC 序列参数集: {} 字节（体前 8 字节 {:02x?}）",
                    param_sets.len(),
                    &body[..8]
                );
                if !param_sets.is_empty() {
                    log::info!(
                        "参数集前 16 字节: {:02x?}",
                        &param_sets[..param_sets.len().min(16)]
                    );
                    if let Err(e) = decoder.decode(&param_sets) {
                        log::warn!("参数集解码失败: {e:?}");
                    }
                }
            }
            1 => {
                // 体布局：flags(1) + AVCPacketType(1) + CompositionTime(3) + AVCC NALU。
                // NoFlush 下参考链持续有效，无需关键帧前重复拼接参数集。
                let annexb = avcc_to_annexb(&body[5..]);
                if annexb.is_empty() {
                    continue;
                }
                // 组合时间（body[2..5]）忽略——显示延迟可接受。
                // 解码错误不静默：持续失败（编码不兼容等）由上层降级清晰度。
                match decoder.decode(&annexb) {
                    Ok(Some(yuv)) => {
                        out_ok += 1;
                        let (w, h) = yuv.dimensions();
                        if w == 0 || h == 0 {
                            continue;
                        }
                        let strides = yuv.strides();
                        let mut f = frames.lock().unwrap_or_else(PoisonError::into_inner);
                        f.next_seq += 1;
                        let frame = Arc::new(frame_from_yuv(
                            f.next_seq,
                            media_ts,
                            w as u32,
                            h as u32,
                            yuv.y(),
                            yuv.u(),
                            yuv.v(),
                            strides.0,
                            strides.1,
                        ));
                        f.latest = Some(frame.clone());
                        // 快进态：按 1/倍率 入队（解码不中断，参考链完整）。
                        let push = match &mut ff {
                            Some((factor, start)) => {
                                ff_tick += 1;
                                if out_lag_ms(anchor, media_ts) < CATCHUP_DONE_MS {
                                    log::info!("直播追帧：快进完成，恢复正常输出");
                                    ff = None;
                                    true
                                } else if start.elapsed() > Duration::from_secs(4) && *factor < 4 {
                                    *factor = 4;
                                    *start = std::time::Instant::now();
                                    log::info!("直播追帧：4 秒未追上，快进倍率升至 4×");
                                    ff_tick.is_multiple_of(4)
                                } else if start.elapsed() > Duration::from_secs(10) {
                                    // 快进 10 秒仍严重落后：转跳帧模式。
                                    skipping = true;
                                    skip_since = Some(std::time::Instant::now());
                                    ff = None;
                                    log::info!("直播追帧：快进超时，转丢弃至下一关键帧");
                                    true
                                } else {
                                    ff_tick.is_multiple_of(*factor)
                                }
                            }
                            None => true,
                        };
                        if push {
                            f.queue.push_back(frame);
                        }
                        while f.queue.len() > FRAME_BUFFER_MAX {
                            f.queue.pop_front();
                        }
                        drop(f);
                    }
                    Ok(None) => out_none += 1,
                    Err(e) => {
                        out_err += 1;
                        dec_fails += 1;
                        if dec_fails == 1 || dec_fails.is_multiple_of(20) {
                            log::warn!("H.264 解码失败（{dec_fails} 次）: {e:?}");
                        }
                        if dec_fails >= DECODE_FAIL_LIMIT {
                            return Err(
                                "H.264 解码持续失败（编码不兼容或解码器能力不足），降级清晰度重试"
                                    .to_string(),
                            );
                        }
                    }
                }
                // NoFlush 下帧留在解码器内部缓冲：用官方推荐的 DecodeFrame2(NULL)
                // 每帧显式取回（保留参考链——周期 flush_remaining 实测会破坏重排，
                // 导致 P 帧大面积失败）。取回失败（无输出）属正常，继续下一帧。
                unsafe {
                    let mut dst = [std::ptr::null_mut::<u8>(); 3];
                    let mut bi = openh264_sys2::SBufferInfo::default();
                    let state = decoder.raw_api().decode_frame2(
                        std::ptr::null(),
                        0,
                        dst.as_mut_ptr().cast(),
                        &mut bi,
                    );
                    if state == 0 && bi.iBufferStatus != 0 && !dst[0].is_null() {
                        let info = bi.UsrData.sSystemBuffer;
                        let (w, h) = (info.iWidth as u32, info.iHeight as u32);
                        if w != 0 && h != 0 {
                            let (ys, us) = (info.iStride[0] as usize, info.iStride[1] as usize);
                            let y = std::slice::from_raw_parts(
                                dst[0] as *const u8,
                                info.iHeight as usize * ys,
                            );
                            let uv_len = info.iHeight as usize * us / 2;
                            let u = std::slice::from_raw_parts(dst[1] as *const u8, uv_len);
                            let v = std::slice::from_raw_parts(dst[2] as *const u8, uv_len);
                            // 输出侧追帧（NULL 取回路径）。
                            if let Some((a_ts, a_at)) = anchor {
                                let expected = a_ts as i64 + a_at.elapsed().as_millis() as i64;
                                let out_lag = expected - media_ts as i64;
                                if out_lag > SEVERE_LAG_MS && !skipping {
                                    // 严重积压：快进也追不上，跳到关键帧硬追。
                                    skipping = true;
                                    skip_since = Some(std::time::Instant::now());
                                    ff = None;
                                    catchup_count += 1;
                                    log::info!(
                                        "直播追帧（严重积压 {out_lag}ms，第 {catchup_count} 次）：丢弃至下一关键帧"
                                    );
                                } else if out_lag > CATCHUP_LAG_MS && ff.is_none() && !skipping {
                                    ff = Some((2, std::time::Instant::now()));
                                    ff_tick = 0;
                                    catchup_count += 1;
                                    log::info!(
                                        "直播追帧（快进 2×，落后 {out_lag}ms，第 {catchup_count} 次）"
                                    );
                                }
                            }
                            let mut f = frames.lock().unwrap_or_else(PoisonError::into_inner);
                            f.next_seq += 1;
                            let frame = Arc::new(frame_from_yuv(
                                f.next_seq, media_ts, w, h, y, u, v, ys, us,
                            ));
                            f.latest = Some(frame.clone());
                            // 快进态入队门控（与 decode() 路径一致）。
                            let push = match &mut ff {
                                Some((factor, _start)) => {
                                    ff_tick += 1;
                                    if out_lag_ms(anchor, media_ts) < CATCHUP_DONE_MS {
                                        ff = None;
                                        true
                                    } else {
                                        ff_tick.is_multiple_of(*factor)
                                    }
                                }
                                None => true,
                            };
                            if push {
                                f.queue.push_back(frame);
                            }
                            while f.queue.len() > FRAME_BUFFER_MAX {
                                f.queue.pop_front();
                            }
                        }
                    }
                }
                // 每 50 个视频 tag 心跳（诊断用）：输入滞后 / 输出滞后 / 队列深度。
                if (out_ok + out_none + out_err) % 50 == 0 {
                    let (qlen, out_ts) = {
                        let f = frames.lock().unwrap_or_else(PoisonError::into_inner);
                        (f.queue.len(), f.latest.as_ref().map(|b| b.ts).unwrap_or(0))
                    };
                    let in_lag = anchor.as_ref().map_or(0, |(a_ts, a_at)| {
                        *a_ts as i64 + a_at.elapsed().as_millis() as i64 - media_ts as i64
                    });
                    let out_lag = anchor.as_ref().map_or(0, |(a_ts, a_at)| {
                        *a_ts as i64 + a_at.elapsed().as_millis() as i64 - out_ts as i64
                    });
                    log::info!(
                        "解码心跳: 出帧 {out_ok} / 无输出 {out_none} / 错误 {out_err}，队列 {qlen}，输入滞后 {in_lag}ms，输出滞后 {out_lag}ms"
                    );
                }
                // 每 300 个视频 tag（约 5 秒）摘要一次解码结果分布（诊断用）。
                if (out_ok + out_none + out_err) % 300 == 0 {
                    log::info!(
                        "解码摘要: 出帧 {out_ok} / 无输出 {out_none} / 错误 {out_err}（当前清晰度 qn）"
                    );
                }
            }
            _ => {}
        }
    }
}
