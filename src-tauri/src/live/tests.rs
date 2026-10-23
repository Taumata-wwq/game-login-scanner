//! live 模块的单元测试：帧管线与流解析的纯逻辑。
//
//! 这些用例覆盖管线里唯一能被确定性验证的部分：字节级转换、尺寸钳制、队列语义与
//! 档位降级链。曾经的三个真实缺陷（队列丢帧、AVCC 越界、档位降级静默失效）都落在
//! 这里，且都不是编译期能发现的。

use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::bili::{build_qn_chain, parse_accept_qn};
use super::decode::push_decoded;
use super::frames::{
    avcc_config_to_annexb, avcc_to_annexb, scan_output_size, Frames, FRAME_BUFFER_MAX,
    SCAN_MAX_LONG_SIDE,
};
use super::preview::{crop_frame, FrameOut};
use crate::decode::DecodedFrame;

fn nal(data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(data);
    v
}

#[test]
fn avcc_converts_multiple_nalus_to_annexb() {
    let mut input = nal(&[0x65, 0xAA, 0xBB]);
    input.extend_from_slice(&nal(&[0x41, 0xCC]));
    let out = avcc_to_annexb(&input);
    assert_eq!(
        out,
        vec![0, 0, 0, 1, 0x65, 0xAA, 0xBB, 0, 0, 0, 1, 0x41, 0xCC]
    );
}

#[test]
fn avcc_empty_input_yields_empty() {
    assert!(avcc_to_annexb(&[]).is_empty());
    assert!(avcc_to_annexb(&[0, 0, 0]).is_empty(), "不足 4 字节不应产出");
}

#[test]
fn avcc_zero_length_nalu_terminates() {
    // 长度为 0 的 NALU 视为损坏：立刻停止，不产出半截数据。
    let mut input = nal(&[0x65]);
    input.extend_from_slice(&[0, 0, 0, 0]);
    input.extend_from_slice(&nal(&[0x41]));
    assert_eq!(avcc_to_annexb(&input), vec![0, 0, 0, 1, 0x65]);
}

#[test]
fn avcc_truncated_nalu_is_dropped() {
    // 声明 4 字节但只剩 2 字节：不能越界，也不能产出残缺 NALU。
    let input = [0u8, 0, 0, 4, 0x65, 0xAA];
    assert!(avcc_to_annexb(&input).is_empty());
}

#[test]
fn avcc_config_extracts_sps_and_pps() {
    // AVCDecoderConfigurationRecord：5 字节头 + (sps_count&0x1F) + [len,sps] +
    // pps_count + [len,pps]
    let sps = [0x67u8, 0x42, 0x00, 0x1F];
    let pps = [0x68u8, 0xCE, 0x3C, 0x80];
    let mut cfg = vec![0x01, 0x42, 0x00, 0x1F, 0xFF, 0xE1];
    cfg.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    cfg.extend_from_slice(&sps);
    cfg.push(0x01);
    cfg.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    cfg.extend_from_slice(&pps);
    let out = avcc_config_to_annexb(&cfg);
    assert_eq!(
        out,
        [&[0, 0, 0, 1][..], &sps[..], &[0, 0, 0, 1][..], &pps[..]].concat()
    );
}

#[test]
fn avcc_config_rejects_short_and_truncated_buffers() {
    assert!(avcc_config_to_annexb(&[]).is_empty());
    assert!(
        avcc_config_to_annexb(&[1, 2, 3, 4, 5, 6]).is_empty(),
        "6 字节不足最小长度"
    );
    // 声明 sps 长度 8，但只给了 2 字节：整段丢弃而不是越界读。
    let cfg = [0x01, 0x42, 0x00, 0x1F, 0xFF, 0xE1, 0x00, 0x08, 0x67, 0x42];
    assert!(avcc_config_to_annexb(&cfg).is_empty());
}

#[test]
fn qn_chain_sorts_desc_dedups_and_falls_back_to_original() {
    assert_eq!(
        build_qn_chain(&[250, 10000, 400, 250]),
        vec![10000, 400, 250]
    );
    assert_eq!(build_qn_chain(&[]), vec![10000], "空列表退化为仅原画");
    assert_eq!(
        build_qn_chain(&[20000]),
        vec![10000],
        "超过 10000 的档位被剔除后回落原画"
    );
    assert_eq!(build_qn_chain(&[150]), vec![150]);
}

#[test]
fn parse_accept_qn_reads_only_http_stream_avc_flv() {
    let v: Value = serde_json::from_str(
    r#"{"data":{"playurl_info":{"playurl":{"stream":[
        {"protocol_name":"http_hls","format":[{"format_name":"ts","codec":[{"codec_name":"avc","accept_qn":[10000,250]}]}]},
        {"protocol_name":"http_stream","format":[{"format_name":"flv","codec":[{"codec_name":"hevc","accept_qn":[10000,400]}]}]},
        {"protocol_name":"http_stream","format":[{"format_name":"flv","codec":[{"codec_name":"avc","accept_qn":[250,10000,400,400]}]}]}
    ]}}}}"#,
)
.unwrap();
    assert_eq!(
        parse_accept_qn(&v),
        vec![10000, 400, 250],
        "降序去重，忽略 hls/hevc"
    );
}

#[test]
fn parse_accept_qn_empty_when_structure_missing() {
    assert!(parse_accept_qn(&serde_json::json!({})).is_empty());
    assert!(parse_accept_qn(&serde_json::json!({"data":{"playurl_info":null}})).is_empty());
}

#[test]
fn scan_output_size_keeps_small_and_scales_large() {
    assert_eq!(scan_output_size(640, 360), (640, 360), "小于上限原样返回");
    let (w, h) = scan_output_size(1920, 1080);
    assert_eq!(w, SCAN_MAX_LONG_SIDE, "长边压到上限");
    assert_eq!(h, 1080 * SCAN_MAX_LONG_SIDE / 1920, "短边按比例");
    let (w2, h2) = scan_output_size(1080, 1920);
    assert_eq!(h2, SCAN_MAX_LONG_SIDE, "竖向流按高度为长边");
    assert_eq!(w2, 1080 * SCAN_MAX_LONG_SIDE / 1920);
    assert_eq!(scan_output_size(0, 0), (1, 1), "零尺寸钳到 1 避免除零");
}

/// 极小尺寸的伪造解码帧：只验证队列/序号语义，不涉及像素内容。
fn tiny_frame() -> DecodedFrame {
    DecodedFrame {
        w: 2,
        h: 2,
        y: vec![0; 4],
        u: vec![0; 1],
        v: vec![0; 1],
    }
}

#[test]
fn queue_keeps_newest_and_drops_oldest_on_overflow() {
    let frames = Arc::new(Mutex::new(Frames {
        queue: std::collections::VecDeque::new(),
        latest: None,
        scan_cache: None,
        next_seq: 0,
    }));
    for _ in 0..(FRAME_BUFFER_MAX + 5) {
        push_decoded(&frames, 0, tiny_frame());
    }
    let f = frames.lock().unwrap();
    assert_eq!(f.queue.len(), FRAME_BUFFER_MAX, "队列封顶，不会无限增长");
    assert_eq!(f.next_seq, (FRAME_BUFFER_MAX + 5) as u64);
    // 丢的是最旧：队首序号应为 6（前 5 帧被挤出），队尾是最新帧。
    assert_eq!(f.queue.front().map(|x| x.seq), Some(6));
    assert_eq!(
        f.queue.back().map(|x| x.seq),
        Some((FRAME_BUFFER_MAX + 5) as u64)
    );
    // latest 始终指向最新帧，扫码路径与显示弹出解耦。
    assert_eq!(
        f.latest.as_ref().map(|x| x.seq),
        Some((FRAME_BUFFER_MAX + 5) as u64)
    );
}

#[test]
fn crop_frame_without_session_reports_no_session() {
    assert!(matches!(
        crop_frame("bilibili:0", 0, 0, 0, 0, 320),
        FrameOut::NoSession
    ));
}
