//! LL-HLS（低延迟 HLS）拉流：m3u8 + fMP4/CMAF 解析。
//!
//! B站部分直播间只提供 HLS 流（http_hls/fmp4），无 FLV。本模块用纯手写解析
//! （零新依赖）把 HLS 还原为「H.264 Annex-B 帧序列」，复用既有 openh264 解码管线。
//!
//! 结构：LL-HLS 用 1 秒分片、fMP4（CMAF）格式：
//! - init 段（EXT-X-MAP）：ftyp+moov，含 avcC（SPS/PPS）与 mdhd（timescale）；
//! - 媒体段：moof（tfhd/tfdt/trun 描述样本）+ mdat（AVCC 长度前缀 NALU）。
//!
//! 逐样本取出 AVCC 转 Annex-B（复用 live::avcc_to_annexb）再解码。

use serde_json::Value;

/// m3u8 里的一个媒体段。
pub struct Segment {
    pub url: String,
    /// 该段序号（MEDIA-SEQUENCE + 索引），用于去重已下载分片。
    pub seq: u64,
    /// 是否关键帧段（EXT-BILI-AUX 标记 |K|）。随机访问须从关键帧段起。
    pub keyframe: bool,
}

/// 解析后的 m3u8 播放列表。
pub struct Playlist {
    pub init_url: Option<String>,
    pub segments: Vec<Segment>,
    pub media_sequence: u64,
    /// 目标分片时长（秒），来自 EXT-X-TARGETDURATION。用于拉流循环按分片边界调度。
    pub target_duration: u64,
}

/// 解析 m3u8 文本；base_url 用于把相对 URI 解析为绝对 URL。
pub fn parse_m3u8(text: &str, base_url: &str) -> Option<Playlist> {
    let mut init_uri: Option<String> = None;
    let mut segments: Vec<Segment> = Vec::new();
    let mut media_sequence: u64 = 0;
    let mut target_duration: u64 = 1;
    let mut next_keyframe = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(u) = extract_attr(rest, "URI") {
                init_uri = Some(resolve(base_url, &u));
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_duration = rest.trim().parse().unwrap_or(1).max(1);
        } else if let Some(rest) = line.strip_prefix("#EXT-BILI-AUX:") {
            // 格式 <hex>|K或N|<size>|<hash>；K 表示该段以关键帧开始。
            next_keyframe = rest.split('|').nth(1).map(|s| s == "K").unwrap_or(false);
        } else if !line.starts_with('#') {
            let idx = segments.len() as u64;
            segments.push(Segment {
                url: resolve(base_url, line),
                seq: media_sequence + idx,
                keyframe: next_keyframe,
            });
            next_keyframe = false;
        }
    }
    if segments.is_empty() && init_uri.is_none() {
        return None;
    }
    Some(Playlist {
        init_url: init_uri,
        segments,
        media_sequence,
        target_duration,
    })
}

/// 从属性串里取 KEY="value"（或 KEY=value）。
fn extract_attr(s: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=");
    let idx = s.find(&pat)?;
    let rest = &s[idx + pat.len()..];
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let end = rest
        .find('"')
        .unwrap_or_else(|| rest.find(',').unwrap_or(rest.len()));
    Some(rest[..end].to_string())
}

/// 把（可能相对的）URI 解析为绝对 URL。
fn resolve(base_url: &str, uri: &str) -> String {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return uri.to_string();
    }
    let (dir, query) = match base_url.rfind('/') {
        Some(i) => {
            let dir = &base_url[..=i];
            let q = base_url[i..]
                .find('?')
                .map(|j| &base_url[i + j..])
                .unwrap_or("");
            (dir.to_string(), q.to_string())
        }
        None => (format!("{base_url}/"), String::new()),
    };
    format!("{dir}{uri}{query}")
}

/// init 段解析结果。
pub struct InitInfo {
    pub params: Vec<u8>,
    pub timescale: u32,
    pub track_id: u32,
}

/// 解析 init 段（ftyp+moov）：提取 avcC（SPS/PPS）与视频轨的 track_id/timescale。
///
/// 关键：**视频轨的 track_id 不固定**（实测有房间视频轨为 1、音频轨为 2；
/// 也有房间反之）。必须从含 avc1 的那条 trak 里读 tkhd 取 track_id，
/// 否则 media segment 会按错误的 id 取到音频轨、解码 0 帧。
pub fn parse_init(data: &[u8]) -> Option<InitInfo> {
    let avcc = find_boxes(data, b"avcC").into_iter().next()?;
    let body = box_body(data, avcc)?;
    let params = crate::live::avcc_config_to_annexb(body);
    if params.is_empty() {
        return None;
    }
    // 遍历所有 trak，找含 avc1 的视频轨，取其 track_id 与 timescale。
    let mut track_id = 1u32;
    let mut timescale = 1000u32;
    for trak in find_boxes(data, b"trak") {
        let Some(tb) = box_body(data, trak) else {
            continue;
        };
        // 只处理含 avc1 的视频轨。
        if find_boxes(tb, b"avc1").is_empty() {
            continue;
        }
        if let Some(tk) = find_boxes(tb, b"tkhd").into_iter().next() {
            if let Some(t) = box_body(tb, tk) {
                track_id = parse_tkhd_track_id(t);
            }
        }
        if let Some(md) = find_boxes(tb, b"mdhd").into_iter().next() {
            if let Some(m) = box_body(tb, md) {
                timescale = parse_mdhd_timescale(m);
            }
        }
        break;
    }
    Some(InitInfo {
        params,
        timescale,
        track_id,
    })
}

/// tkhd box 体 → track_id（version 0: [12..16]，version 1: [20..24]）。
fn parse_tkhd_track_id(body: &[u8]) -> u32 {
    if body.len() < 4 {
        return 1;
    }
    let version = body[0];
    let off = if version == 1 { 20 } else { 12 };
    if body.len() >= off + 4 {
        u32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]])
    } else {
        1
    }
}

/// mdhd box 体转 timescale（version 0: [12..16]，version 1: [20..24]）。
fn parse_mdhd_timescale(body: &[u8]) -> u32 {
    if body.len() < 4 {
        return 1000;
    }
    let version = body[0];
    let off = if version == 1 { 20 } else { 12 };
    if body.len() >= off + 4 {
        u32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]])
    } else {
        1000
    }
}

/// 媒体段里的一个视频样本。
pub struct Sample {
    /// 段内字节偏移。
    pub offset: usize,
    /// 字节长度。
    pub size: usize,
    /// 绝对媒体时间（timescale 单位，来自 tfdt + 累计时长）。
    pub time: u64,
}

/// 解析媒体段（含多个 moof+mdat）：提取指定轨的全部样本。
/// 一个分片通常含多个 moof（每个约 8 帧），须全部收集。
pub fn parse_segment(data: &[u8], track_id: u32) -> Vec<Sample> {
    let mut out = Vec::new();
    for moof in find_boxes(data, b"moof") {
        let Some(moof_body) = box_body(data, moof) else {
            continue;
        };
        for traf in find_boxes(moof_body, b"traf") {
            let Some(tb) = box_body(moof_body, traf) else {
                continue;
            };
            let Some(tfhd_box) = find_boxes(tb, b"tfhd").into_iter().next() else {
                continue;
            };
            let Some(tfhd) = box_body(tb, tfhd_box) else {
                continue;
            };
            let (tfhd_flags, tf_track, default_size) = parse_tfhd(tfhd);
            if tf_track != track_id {
                continue;
            }
            let base_time = find_boxes(tb, b"tfdt")
                .into_iter()
                .next()
                .and_then(|d| box_body(tb, d))
                .map(parse_tfdt)
                .unwrap_or(0);
            let Some(trun_box) = find_boxes(tb, b"trun").into_iter().next() else {
                continue;
            };
            let Some(trun) = box_body(tb, trun_box) else {
                continue;
            };
            let Some(samples) = parse_trun(trun, moof.0, tfhd_flags, default_size) else {
                continue;
            };
            let mut t = base_time;
            for (offset, size, dur) in samples {
                out.push(Sample {
                    offset,
                    size,
                    time: t,
                });
                t += dur as u64;
            }
        }
    }
    out
}

/// tfhd 转 (flags, track_id, default_sample_size)。
fn parse_tfhd(body: &[u8]) -> (u32, u32, Option<usize>) {
    if body.len() < 8 {
        return (0, 0, None);
    }
    let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
    let mut i = 4;
    let track = u32::from_be_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]);
    i += 4;
    let mut default_size = None;
    if flags & 0x1 != 0 {
        i += 8;
    }
    if flags & 0x2 != 0 {
        i += 4;
    }
    if flags & 0x8 != 0 {
        i += 4;
    }
    if flags & 0x10 != 0 && body.len() >= i + 4 {
        default_size = Some(u32::from_be_bytes([
            body[i], body[i + 1], body[i + 2], body[i + 3],
        ]) as usize);
    }
    (flags, track, default_size)
}

/// tfdt 转 baseMediaDecodeTime（version 0: u32，version 1: u64）。
fn parse_tfdt(body: &[u8]) -> u64 {
    if body.len() < 4 {
        return 0;
    }
    let version = body[0];
    if version == 1 {
        if body.len() >= 12 {
            let mut v = 0u64;
            for &b in &body[4..12] {
                v = (v << 8) | b as u64;
            }
            return v;
        }
    } else if body.len() >= 8 {
        return u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as u64;
    }
    0
}

/// trun 转样本 (偏移, 长度, 时长)。moof_start 为 moof box 在段内的起始偏移
/// （trun 的 data_offset 相对 moof 起始）。
fn parse_trun(
    body: &[u8],
    moof_start: usize,
    _tfhd_flags: u32,
    default_size: Option<usize>,
) -> Option<Vec<(usize, usize, u32)>> {
    if body.len() < 8 {
        return None;
    }
    let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
    let count = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as usize;
    let mut i = 8;
    let mut data_offset: i64 = 0;
    if flags & 0x1 != 0 {
        if body.len() < i + 4 {
            return None;
        }
        data_offset = i32::from_be_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]) as i64;
        i += 4;
    }
    if flags & 0x4 != 0 {
        i += 4;
    }
    let mut cur = (moof_start as i64 + data_offset) as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let mut dur = 0u32;
        let mut size = default_size.unwrap_or(0);
        if flags & 0x100 != 0 {
            dur = u32::from_be_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]);
            i += 4;
        }
        if flags & 0x200 != 0 {
            size = u32::from_be_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]) as usize;
            i += 4;
        }
        if flags & 0x400 != 0 {
            i += 4;
        }
        if flags & 0x800 != 0 {
            i += 4;
        }
        out.push((cur, size, dur));
        cur += size;
    }
    Some(out)
}

/// 取 box 的 body（跳过 8 字节 header）。box 为 (起始偏移, 总长度)。
fn box_body(data: &[u8], b: (usize, usize)) -> Option<&[u8]> {
    let start = b.0 + 8;
    let end = b.0 + b.1;
    if start <= end && end <= data.len() {
        Some(&data[start..end])
    } else {
        None
    }
}

/// 在 data 里递归查找 typ box，返回 (偏移, 长度)。
fn find_boxes(data: &[u8], typ: &[u8; 4]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    scan_boxes(data, 0, data.len(), typ, &mut out);
    out
}

fn scan_boxes(data: &[u8], off: usize, end: usize, typ: &[u8; 4], out: &mut Vec<(usize, usize)>) {
    let mut i = off;
    while i + 8 <= end {
        let size32 = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let btype = &data[i + 4..i + 8];
        let (size, hdr) = if size32 == 1 {
            if i + 16 > end {
                break;
            }
            let mut v = 0u64;
            for &b in &data[i + 8..i + 16] {
                v = (v << 8) | b as u64;
            }
            (v as usize, 16)
        } else if size32 == 0 {
            (end - i, 8)
        } else {
            (size32, 8)
        };
        if size < hdr || i + size > end {
            break;
        }
        if btype == typ {
            out.push((i, size));
        }
        if is_container(btype) {
            let mut inner = i + hdr;
            if btype == b"stsd" {
                inner += 8;
            } else if btype == b"avc1" {
                inner += 78;
            }
            if inner < i + size {
                scan_boxes(data, inner, i + size, typ, out);
            }
        }
        i += size;
    }
}

fn is_container(t: &[u8]) -> bool {
    matches!(
        t,
        b"moov"
            | b"trak"
            | b"mdia"
            | b"minf"
            | b"stbl"
            | b"stsd"
            | b"avc1"
            | b"mvex"
            | b"moof"
            | b"traf"
    )
}

/// 从 getRoomPlayInfo 响应提取 HLS m3u8 地址（http_hls / fmp4 / avc）。
pub fn parse_bili_hls(v: &Value) -> Option<String> {
    let streams = v["data"]["playurl_info"]["playurl"]["stream"].as_array()?;
    for stream in streams {
        if stream["protocol_name"].as_str() != Some("http_hls") {
            continue;
        }
        for fmt in stream["format"].as_array()? {
            if fmt["format_name"].as_str() != Some("fmp4") {
                continue;
            }
            for codec in fmt["codec"].as_array()? {
                if codec["codec_name"].as_str() == Some("avc") {
                    if let Some(u) = crate::live::codec_url(codec) {
                        return Some(u);
                    }
                }
            }
        }
    }
    None
}
