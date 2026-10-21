//! DXGI 桌面复制（Desktop Duplication）低开销屏幕采集。
//!
//! 架构：**帧总线线程**（每进程一个）持有每块显示器的 `IDXGIOutputDuplication`，
//! 以 60Hz 轮询 `AcquireNextFrame(0)`：
//! - 画面静止：立即返回超时（微秒级），CPU 接近零；
//! - 画面变化：仅做一次 GPU→GPU `CopyResource` 到 staging 纹理（不落 CPU），
//!   并递增该显示器的 `frame_id`；
//! - 消费者（预览 / 扫码）只在 `frame_id` 变化时才 `Map` staging 取数据：
//!   预览只抽点读取约 360px 缩略图（<1ms），扫码做整帧 BGRA→RGB 转换。
//!
//! 同时解决两个问题：
//! 1. GDI BitBlt 每帧全屏拷贝 + DWM 争用（旧预览 15fps 即吃 20% CPU）；
//! 2. 扫码循环对静止屏幕的重复截屏（登录界面背景动画会持续触发截屏解码）。
//!
//! 复制不可用（安全桌面 / HDR 格式 / 设备丢失）时返回 `Err`，调用方走 GDI 兜底。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_MOVE_RECT, DXGI_OUTPUT_DESC,
};

/// 显示器矩形键（虚拟桌面物理像素 x/y/w/h）。
pub type MonitorKey = (i32, i32, i32, i32);

/// 瓦片像素尺寸（在整屏降采样图上）。64 → 1280×720 时为 20×12=240 块。
const TILE_PX: u32 = 64;

/// 单个变化的瓦片：(col, row, w, h, RGBA)。
pub type TileUpdate = (u32, u32, u32, u32, Vec<u8>);
/// 瓦片增量帧：(降采样宽, 高, 变化瓦片列表)。
pub type TileDelta = (u32, u32, Vec<TileUpdate>);

/// 瓦片（拖拽底图）模式状态：维护一块**持久整屏降采样底图**，只对脏瓦片更新。
/// 拖拽时屏幕内容大多静止 → 仅少数瓦片变化 → IPC 传输从整屏(≈3.7MB)降到几十 KB。
struct TileState {
    /// 整屏降采样尺寸
    tw: u32,
    th: u32,
    /// 持久整屏 RGBA 底图（tw×th）
    base: Vec<u8>,
    /// 本帧待更新瓦片（来自脏矩形）
    dirty: Vec<bool>,
    /// 网格
    cols: u32,
    rows: u32,
}

impl TileState {
    fn new() -> Self {
        TileState {
            tw: 0,
            th: 0,
            base: Vec::new(),
            dirty: Vec::new(),
            cols: 0,
            rows: 0,
        }
    }
    /// 尺寸变化时重建缓冲（全部标脏）。
    fn resize(&mut self, tw: u32, th: u32) {
        let cols = tw.div_ceil(TILE_PX);
        let rows = th.div_ceil(TILE_PX);
        let n = (cols * rows) as usize;
        self.tw = tw;
        self.th = th;
        self.cols = cols;
        self.rows = rows;
        self.base = vec![0u8; (tw * th * 4) as usize];
        self.dirty = vec![true; n];
    }
}

/// 每块显示器的复制会话。锁内访问；总线线程与消费者共享。
struct MonitorDup {
    /// DXGI 输出矩形（虚拟桌面物理坐标），裁剪换算基准。
    output_rect: MonitorKey,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    w: u32,
    h: u32,
    /// 最近采集到的新帧序号（总线线程递增，staging 内容与之对应）。
    frame_id: u64,
    /// 扫码消费者已见序号，**按降采样倍率分桶**（下标 = scale）。
    ///
    /// 为什么必须分桶：主档（1/2 步长）与兜底档（全分辨率）是对同一帧的两次不同
    /// 尺寸消费；若共用一个计数器，主档先消费即把该帧标为「已见」，随后兜底档
    /// 会误判并取不到全分辨率帧（静默失效）。
    scan_seen: [u64; 3],
    /// 预览消费者已见序号与请求（宽度 + 区域，缓存命中判定）。
    prev_seen: u64,
    prev_tw: u32,
    prev_region: Option<MonitorKey>,
    /// 上次预览降采样结果的 FNV-1a 哈希：裁剪区内容未变时跳过发送与重绘。
    prev_hash: u64,
    /// 活跃消费者关注区（桌面绝对坐标）：预览视口 / 扫码区域。
    /// 总线据此用脏矩形判断「本次变化是否落在关注区内」，不在则跳过全屏拷贝。
    preview_roi: Option<MonitorKey>,
    scan_roi: Option<MonitorKey>,
    /// 关注区刚变化：下一帧强制拷贝（否则过滤会读到与新旧视口都不匹配的陈旧 staging）。
    roi_dirty: bool,
    /// staging 中当前已拷贝区域的裁剪矩形（输出相对坐标，落在 staging 左上角）。
    /// None 表示尚无有效拷贝。消费者据此把请求区域换算为 staging 内坐标。
    copied: Option<(u32, u32, u32, u32)>,
    /// 面积平均降采样的复用累加缓冲（避免每帧分配数 MB）。
    avg_sums: Vec<u32>,
    avg_counts: Vec<u16>,
    /// 瓦片（拖拽底图）模式状态。
    tiles: TileState,
    /// 是否启用瓦片模式（拖拽期间由前端开启）。
    tile_mode: bool,
    /// 消费者是否请求了新帧：总线只在为 true 时拷贝（消费者 30Hz 读取，
    /// 避免总线 60Hz 拷贝一半被丢弃），拷完置回 false。
    tile_want: bool,
    /// 本次瓦片模式会话需要「无条件全量重发」：前端每次开始拖拽都会重建底图
    /// canvas（内容为空），而后端 `tiles.base` 跨会话保留、内容比对会跳过未变化
    /// 瓦片——只发增量会让前端新画布的大片区域停留在透明（黑块）。置位后
    /// `tile_delta` 跳过内容比对，把全部瓦片发一遍（一次全量 pass 完成后复位）。
    tiles_fresh: bool,
}

/// staging 的只读 Map 守卫：Drop 时 Unmap（持有 COM 引用计数，不借 MonitorDup）。
/// 异常展开时也能 Unmap，避免 staging 停在已 Map 状态导致后续 Map 恒失败。
struct MappedStaging {
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
    ptr: *const u8,
    pitch: usize,
}

impl Drop for MappedStaging {
    fn drop(&mut self) {
        unsafe { self.context.Unmap(&self.texture, 0) };
    }
}

/// 两个关注区的并集（都为 None 时返回 None）。
fn union_roi(a: Option<MonitorKey>, b: Option<MonitorKey>) -> Option<MonitorKey> {
    match (a, b) {
        (None, None) => None,
        (Some(r), None) | (None, Some(r)) => Some(r),
        (Some(a), Some(b)) => {
            let x0 = a.0.min(b.0);
            let y0 = a.1.min(b.1);
            let x1 = (a.0 + a.2).max(b.0 + b.2);
            let y1 = (a.1 + a.3).max(b.1 + b.3);
            Some((x0, y0, x1 - x0, y1 - y0))
        }
    }
}

/// BGRA（行跨距 `pitch` 字节）→ **步长采样灰度**：从 `(cx, cy)` 起取 `(cw, ch)`
/// 区域，每 `scale` 个像素取首点，输出尺寸 `(cw/scale, ch/scale)`。
///
/// 与「先对 `(cw, ch)` 求全分辨率灰度、再按 `scale` 步长降采样」**逐位一致**
/// （同一个源像素的同一个亮度值），因此识别行为不变；但把两趟并成一趟后，
/// 主档（`scale=2`）的转换量降到 1/4 —— 实测 2569x1445 由 15.1ms 降到 3.6ms、
/// 1282x747 由 5.8ms 降到 1.8ms（`tests/bench_qr_screen.rs`
/// `bench_screen_capture_convert_cost`）。
///
/// 灰度权重与 `qr` 侧 `(77R+151G+28B)>>8` 完全一致。
///
/// 调用方须保证 `(cx+cw, cy+ch)` 落在 `bgra` 覆盖的行内（越界会 panic）。
pub fn bgra_gray_stride(
    bgra: &[u8],
    pitch: usize,
    cx: usize,
    cy: usize,
    cw: u32,
    ch: u32,
    scale: usize,
) -> (Vec<u8>, u32, u32) {
    let scale = scale.clamp(1, 2);
    let ow = (cw as usize / scale).max(1);
    let oh = (ch as usize / scale).max(1);
    let mut out = vec![0u8; ow * oh];
    let mut oi = 0usize;
    for oy in 0..oh {
        let row = &bgra[(cy + oy * scale) * pitch + cx * 4..];
        for ox in 0..ow {
            // 按「输出列数」迭代并自乘步长（`(0..cw).step_by(scale)` 在宽度非整除时
            // 迭代次数会比 `cw/scale` 多 1，越界写会 panic 或踩内存）。
            let i = ox * scale * 4;
            // BGRA：i+2=R, i+1=G, i=B。
            out[oi] = ((row[i + 2] as u32 * 77 + row[i + 1] as u32 * 151 + row[i] as u32 * 28) >> 8)
                as u8;
            oi += 1;
        }
    }
    (out, ow as u32, oh as u32)
}

/// FNV-1a 64 位哈希（对降采样后的 RGBA 缓冲，做快速内容指纹）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

enum Grab {
    /// 采集到新帧（已拷贝到 staging）
    New,
    /// 自上次以来无新帧
    NoChange,
    /// 会话失效（设备丢失 / 分辨率变化），需重建
    Broken,
}

impl MonitorDup {
    /// 把 key（显示器或区域矩形）换算为输出内裁剪矩形。
    fn crop_of(&self, key: MonitorKey) -> (u32, u32, u32, u32) {
        // clamp 在 min > max 时会 panic，所以先把退化的输出尺寸收敛掉
        // （DXGI 模式理论上非退化，create_monitor 也会另行拒绝零宽高）。
        if self.w == 0 || self.h == 0 {
            return (0, 0, 1, 1);
        }
        let x = (key.0 - self.output_rect.0).clamp(0, self.w as i32 - 1) as u32;
        let y = (key.1 - self.output_rect.1).clamp(0, self.h as i32 - 1) as u32;
        let w = (key.2 as u32).clamp(1, self.w - x);
        let h = (key.3 as u32).clamp(1, self.h - y);
        (x, y, w, h)
    }

    /// 总线线程：采集新帧到 staging（无 CPU 转换）。
    fn grab_new(&mut self) -> Grab {
        // 瓦片模式下消费者未请求新帧：直接跳过（连 AcquireNextFrame 都不做），
        // 省去无谓的 GPU 拷贝——消费者 30Hz 读取，总线不再空转 60Hz。
        if self.tile_mode && !self.tile_want {
            return Grab::NoChange;
        }
        unsafe {
            // 分辨率变化 → 重建。
            let dd = self.dup.GetDesc();
            if dd.ModeDesc.Width != self.w || dd.ModeDesc.Height != self.h {
                return Grab::Broken;
            }
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            match self.dup.AcquireNextFrame(0, &mut info, &mut res) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Grab::NoChange,
                Err(_) => return Grab::Broken,
            }
            let Some(res) = res else { return Grab::Broken };
            let Ok(tex) = res.cast::<ID3D11Texture2D>() else {
                return Grab::Broken;
            };

            // 瓦片模式（拖拽底图）：只拷贝脏瓦片的源包围盒（而非整屏），GPU 拷贝量与
            // 变化面积成正比——拖拽时远小于整屏，显著降低 GPU 带宽占用。
            if self.tile_mode {
                self.mark_dirty_tiles();
                let Some((bx, by, bw, bh)) = self.dirty_src_bbox() else {
                    // 无脏瓦片：无需拷贝。
                    let _ = self.dup.ReleaseFrame();
                    return Grab::NoChange;
                };
                let src_box = D3D11_BOX {
                    left: bx,
                    top: by,
                    front: 0,
                    right: bx + bw,
                    bottom: by + bh,
                    back: 1,
                };
                self.context.CopySubresourceRegion(
                    &self.staging,
                    0,
                    0,
                    0,
                    0,
                    &tex,
                    0,
                    Some(&src_box),
                );
                self.copied = Some((bx, by, bw, bh));
                self.tile_want = false; // 已为该请求拷贝，等消费者下次请求
                let _ = self.dup.ReleaseFrame();
                return Grab::New;
            }

            // ROI 刚变化（含拖拽/缩放的每一帧）：整屏 CopyResource，保证任意区域都可读，
            // 消费者不会因「请求区超出已拷贝区」而跳帧——这消除了拖拽时的卡顿。
            if self.roi_dirty {
                self.roi_dirty = false;
                self.context.CopyResource(&self.staging, &tex);
                self.copied = Some((0, 0, self.w, self.h));
                let _ = self.dup.ReleaseFrame();
                return Grab::New;
            }

            // ROI 稳定：脏矩形过滤 + 按区域拷贝（CopySubresourceRegion），拷贝量与 ROI
            // 面积成正比——放大看局部、画面持续运动时显著降低 GPU 占用。
            let Some(roi) = union_roi(self.preview_roi, self.scan_roi) else {
                // 无活跃关注区：无需拷贝。
                let _ = self.dup.ReleaseFrame();
                return Grab::NoChange;
            };
            if !self.dirty_intersects(roi) {
                let _ = self.dup.ReleaseFrame();
                return Grab::NoChange;
            }
            let (cx, cy, cw, ch) = self.crop_of(roi);
            let src_box = D3D11_BOX {
                left: cx,
                top: cy,
                front: 0,
                right: cx + cw,
                bottom: cy + ch,
                back: 1,
            };
            self.context
                .CopySubresourceRegion(&self.staging, 0, 0, 0, 0, &tex, 0, Some(&src_box));
            self.copied = Some((cx, cy, cw, ch));
            let _ = self.dup.ReleaseFrame();
            Grab::New
        }
    }

    /// 请求区域在 staging 内的坐标（相对已拷贝区左上角）；未被覆盖时返回 None
    /// （此时本帧跳过，`roi_dirty` 会驱动下一帧拷到正确区域）。
    fn staged_crop(&self, key: MonitorKey) -> Option<(u32, u32, u32, u32)> {
        let (ccx, ccy, ccw, cch) = self.copied?;
        let kx = key.0 - self.output_rect.0;
        let ky = key.1 - self.output_rect.1;
        if kx < ccx as i32 || ky < ccy as i32 {
            return None;
        }
        let sx = (kx - ccx as i32) as u32;
        let sy = (ky - ccy as i32) as u32;
        let sw = key.2.max(1) as u32;
        let sh = key.3.max(1) as u32;
        if sx + sw > ccw || sy + sh > cch {
            return None;
        }
        Some((sx, sy, sw, sh))
    }

    /// 本帧脏矩形是否与关注区（桌面绝对坐标）相交。取脏矩形失败时保守返回 true
    /// （宁可多拷贝，不可漏更新）。脏矩形是相对输出左上角的坐标。
    fn dirty_intersects(&self, roi: MonitorKey) -> bool {
        unsafe {
            let mut rects = [windows::Win32::Foundation::RECT::default(); 64];
            let mut required: u32 = 0;
            let cap =
                (rects.len() * std::mem::size_of::<windows::Win32::Foundation::RECT>()) as u32;
            if self
                .dup
                .GetFrameDirtyRects(cap, rects.as_mut_ptr(), &mut required)
                .is_err()
            {
                // 脏矩形不可用（某些驱动/叠加层）或缓冲不足：保守视为相交。
                return true;
            }
            let n = (required as usize) / std::mem::size_of::<windows::Win32::Foundation::RECT>();
            if n == 0 {
                // 无脏矩形（可能仅指针/元数据更新）：保守视为相交。
                return true;
            }
            let (ol, ot) = (self.output_rect.0, self.output_rect.1);
            let rl = roi.0 - ol;
            let rt = roi.1 - ot;
            let rr = rl + roi.2;
            let rb = rt + roi.3;
            rects[..n.min(rects.len())]
                .iter()
                .any(|r| r.left < rr && r.right > rl && r.top < rb && r.bottom > rt)
        }
    }

    /// 当前脏瓦片集合对应的源图包围盒（相对输出左上角，物理像素）。
    /// 无脏瓦片或瓦片网格未初始化时返回 None。
    fn dirty_src_bbox(&self) -> Option<(u32, u32, u32, u32)> {
        let (cols, rows, tw, th) = (
            self.tiles.cols,
            self.tiles.rows,
            self.tiles.tw,
            self.tiles.th,
        );
        if cols == 0 || rows == 0 || tw == 0 || th == 0 || self.w == 0 || self.h == 0 {
            return None;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        let mut any = false;
        for r in 0..rows {
            for c in 0..cols {
                let idx = (r * cols + c) as usize;
                if !self.tiles.dirty.get(idx).copied().unwrap_or(false) {
                    continue;
                }
                let dx0 = c * TILE_PX;
                let dy0 = r * TILE_PX;
                let dw = (tw - dx0).min(TILE_PX);
                let dh = (th - dy0).min(TILE_PX);
                let sx0 = (dx0 as u64 * self.w as u64 / tw as u64) as u32;
                let sy0 = (dy0 as u64 * self.h as u64 / th as u64) as u32;
                let sx1 = ((dx0 + dw) as u64 * self.w as u64 / tw as u64).min(self.w as u64) as u32;
                let sy1 = ((dy0 + dh) as u64 * self.h as u64 / th as u64).min(self.h as u64) as u32;
                x0 = x0.min(sx0);
                y0 = y0.min(sy0);
                x1 = x1.max(sx1.max(sx0 + 1));
                y1 = y1.max(sy1.max(sy0 + 1));
                any = true;
            }
        }
        if !any {
            return None;
        }
        let x1 = x1.min(self.w);
        let y1 = y1.min(self.h);
        Some((
            x0,
            y0,
            x1.saturating_sub(x0).max(1),
            y1.saturating_sub(y0).max(1),
        ))
    }

    /// 用脏矩形标记本帧待更新的瓦片。取脏矩形失败时保守标记全部。
    /// 脏矩形是相对输出左上角的坐标；瓦片网格基于整屏降采样尺寸。
    fn mark_dirty_tiles(&mut self) {
        let (cols, tw, th) = (self.tiles.cols, self.tiles.tw, self.tiles.th);
        if cols == 0 || tw == 0 || th == 0 || self.w == 0 || self.h == 0 {
            return;
        }
        let (gw, gh) = (self.w, self.h);
        // 输出坐标矩形（物理像素）→ 瓦片范围并标脏；脏矩形与移动矩形复用。
        let mark_rect = |dirty: &mut [bool], l: i32, t: i32, r: i32, b: i32| {
            let dx0 = (l.max(0) as u32 * tw / gw).min(tw.saturating_sub(1));
            let dy0 = (t.max(0) as u32 * th / gh).min(th.saturating_sub(1));
            let dx1 = ((r.max(0) as u32) * tw / gw).max(dx0 + 1).min(tw);
            let dy1 = ((b.max(0) as u32) * th / gh).max(dy0 + 1).min(th);
            let c0 = dx0 / TILE_PX;
            let r0 = dy0 / TILE_PX;
            let c1 = (dx1 - 1) / TILE_PX;
            let r1 = (dy1 - 1) / TILE_PX;
            for rr in r0..=r1 {
                for cc in c0..=c1 {
                    let idx = (rr * cols + cc) as usize;
                    if idx < dirty.len() {
                        dirty[idx] = true;
                    }
                }
            }
        };
        // 源图（物理像素）→ 瓦片网格的映射：源 x → 降采样 x = x*tw/w。
        unsafe {
            let mut rects = [windows::Win32::Foundation::RECT::default(); 64];
            let mut required: u32 = 0;
            let cap =
                (rects.len() * std::mem::size_of::<windows::Win32::Foundation::RECT>()) as u32;
            if self
                .dup
                .GetFrameDirtyRects(cap, rects.as_mut_ptr(), &mut required)
                .is_err()
            {
                self.tiles.dirty.iter_mut().for_each(|d| *d = true);
                return;
            }
            let n = (required as usize) / std::mem::size_of::<windows::Win32::Foundation::RECT>();
            // 注意：n == 0 时不能直接返回——滚动/拖动窗口只产生「移动矩形」而无脏矩形，
            // 漏处理会令瓦片底图停在陈旧内容（P0-5）。故继续处理 MoveRects。
            for r in rects[..n.min(rects.len())].iter() {
                mark_rect(&mut self.tiles.dirty, r.left, r.top, r.right, r.bottom);
            }
            // 移动矩形：SourcePoint 处的像素被搬到 DestinationRect。两处都会变化，
            // 保守各标一次（源区留旧内容、目标区是新内容）。取失败时按无移动处理
            // （脏矩形已覆盖主要变化；不额外全标以免拖窗时整屏重传）。
            let mut moves = [DXGI_OUTDUPL_MOVE_RECT::default(); 64];
            let mut mreq: u32 = 0;
            let mcap = (moves.len() * std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>()) as u32;
            if self
                .dup
                .GetFrameMoveRects(mcap, moves.as_mut_ptr(), &mut mreq)
                .is_ok()
            {
                let mn = (mreq as usize) / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>();
                for mr in moves[..mn.min(moves.len())].iter() {
                    let d = mr.DestinationRect;
                    mark_rect(&mut self.tiles.dirty, d.left, d.top, d.right, d.bottom);
                    let dw = d.right - d.left;
                    let dh = d.bottom - d.top;
                    let s = mr.SourcePoint;
                    mark_rect(&mut self.tiles.dirty, s.x, s.y, s.x + dw, s.y + dh);
                }
            }
        }
    }

    /// Map staging 为只读并返回 RAII 守卫；Drop 时保证 Unmap（含 panic 展开路径）。
    fn map_staging(&self) -> Result<MappedStaging, ()> {
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|_| ())?;
            Ok(MappedStaging {
                context: self.context.clone(),
                texture: self.staging.clone(),
                ptr: mapped.pData as *const u8,
                pitch: mapped.RowPitch as usize,
            })
        }
    }

    /// 消费者：Map staging 并执行 `f`（GPU 数据仅在此期间可读）。
    fn with_mapped<R>(&mut self, f: impl FnOnce(*const u8, usize) -> R) -> Result<R, ()> {
        let m = self.map_staging()?;
        Ok(f(m.ptr, m.pitch))
    }

    /// 扫码取帧：仅当出现新帧时转换，返回**按 scale 步长采样**的灰度（1 字节/像素）。
    ///
    /// 扫码只依赖亮度：直接产出灰度省去「RGB 三通道写入 + 二次 RGB→灰度遍历」；
    /// 而把降采样**合并进转换这一趟**（每 scale 个源像素取首点）再省掉
    /// 「全分辨率灰度 + 第二趟步长降采样」——输出与「先全分辨率灰度再步长降采样」
    /// **逐位一致**（同一个源像素的同一个亮度值），识别行为不变，但主档（scale=2）
    /// 的转换量降到 1/4，且不再申请全分辨率灰度缓冲。
    ///
    /// 权重与 `qr` 侧 `(77R+151G+28B)>>8` 完全一致。
    ///
    /// `scale`：降采样倍率（1 = 原分辨率；2 = 1/2 步长，即生产主档）。
    fn scan_frame(
        &mut self,
        key: MonitorKey,
        scale: u32,
    ) -> Result<Option<crate::screenshot::GrayFrame>, ()> {
        let scale = scale.clamp(1, 2);
        if self.scan_roi != Some(key) {
            self.scan_roi = Some(key);
            self.roi_dirty = true;
        }
        if self.scan_seen[scale as usize] == self.frame_id {
            return Ok(None);
        }
        // 请求区域未被本帧 staging 覆盖（ROI 刚变）时跳过，下一帧补上。
        let Some((cx, cy, cw, ch)) = self.staged_crop(key) else {
            return Ok(None);
        };
        let (fh, fcy) = (self.h as usize, cy as usize);
        let frame = self.with_mapped(|ptr, pitch| {
            // 整块 staging（`fh` 行 × `pitch` 字节）当作切片：把热循环抽成纯函数，
            // 既省掉每行的临时切片构造，也让「与旧实现逐位一致」可被单元测试锁定。
            let bgra = unsafe { std::slice::from_raw_parts(ptr, pitch * fh) };
            let (gray, w, h) =
                bgra_gray_stride(bgra, pitch, cx as usize, fcy, cw, ch, scale as usize);
            crate::screenshot::GrayFrame { gray, w, h }
        })?;
        self.scan_seen[scale as usize] = self.frame_id;
        Ok(Some(frame))
    }

    /// 预览取帧：帧号与请求宽度都没变时返回缓存命中。
    /// 降采样用**面积平均**（box filter）——最近邻采样在内容微动时
    /// 逐帧跳变采样点，是预览「静止却抖动/闪烁」的根源。
    fn preview_frame(
        &mut self,
        region: MonitorKey,
        tw: u32,
    ) -> Result<Option<(Vec<u8>, u32, u32)>, ()> {
        if self.preview_roi != Some(region) {
            self.preview_roi = Some(region);
            self.roi_dirty = true;
        }
        if self.prev_seen == self.frame_id && self.prev_tw == tw && self.prev_region == Some(region)
        {
            return Ok(None);
        }
        // 请求视口未被本帧 staging 覆盖（缩放/平移致 ROI 刚变）时跳过，下一帧补上。
        let Some((cx, cy, cw, ch)) = self.staged_crop(region) else {
            return Ok(None);
        };
        // 目标宽度不超过裁剪区实际像素数：面积平均降采样器不支持上采样——
        // tw > cw 时部分目标列没有源像素落入（counts=0），保持初值 0 → 渲染为
        // 黑色网格线（放大到局部最大时尤为明显）。钳制后既消除黑网格，又减少
        // IPC 传输与降采样计算量（降占用）。
        let tw = tw.min(cw);
        let th = ((ch as u64 * tw as u64) / cw as u64).max(1) as u32;
        // Map/Unmap 内联（闭包内还要可变借用 self 的累加缓冲，不能走 with_mapped）。
        // staging 是 CPU 可读纹理，Map 极快，无需 DO_NOT_WAIT（后者在 GPU 仍绘制时
        // 返回空帧，且异常路径会触发 GDI 回退，反而更慢）。
        let mapped = self.map_staging()?;
        let rgba = self.area_average_rgba(mapped.ptr, mapped.pitch, (cx, cy, cw, ch), tw, th);
        drop(mapped);
        // 裁剪区内容哈希去重：桌面其他位置变化（frame_id 递增）导致重算时，
        // 若本视口降采样结果与上次完全一致，则返回「无变化」，避免无谓的
        // IPC 传输与前端重绘（放大后看局部、而全局仍在动时尤为常见）。
        let h = fnv1a(&rgba);
        let same_region = self.prev_region == Some(region) && self.prev_tw == tw;
        self.prev_seen = self.frame_id;
        self.prev_tw = tw;
        self.prev_region = Some(region);
        if same_region && self.prev_hash == h {
            return Ok(None);
        }
        self.prev_hash = h;
        Ok(Some((rgba, tw, th)))
    }

    /// 面积平均降采样：每个输出像素 = 源图中对应箱内全部像素的均值。
    /// 单次遍历源图 + x 箱查找表；累加缓冲挂在 self 上复用。
    fn area_average_rgba(
        &mut self,
        ptr: *const u8,
        pitch: usize,
        crop: (u32, u32, u32, u32),
        tw: u32,
        th: u32,
    ) -> Vec<u8> {
        let (cx, cy, cw, ch) = crop;
        let (tw, th) = (tw as usize, th as usize);
        let n = tw * th;
        self.avg_sums.clear();
        self.avg_sums.resize(n * 3, 0);
        self.avg_counts.clear();
        self.avg_counts.resize(n, 0);
        let sums = &mut self.avg_sums;
        let counts = &mut self.avg_counts;
        // x 箱查找表（每源列一次除法）
        let xbin: Vec<usize> = (0..cw as usize).map(|i| i * tw / cw as usize).collect();
        // 源远大于目标时按步进采样：每个输出箱约取 2 个源点（输出已高度模糊，多点
        // 平均无视觉差异），CPU 降至约 1/(step²)。缩放比越大（看全屏时）步进越大。
        let x_step = (cw as usize).div_ceil(tw).max(1).div_ceil(2).max(1);
        let y_step = (ch as usize).div_ceil(th).max(1).div_ceil(2).max(1);
        for sy in (0..ch as usize).step_by(y_step) {
            let yb = sy * th / ch as usize;
            let base = yb * tw;
            let row = unsafe {
                std::slice::from_raw_parts(
                    ptr.add((cy as usize + sy) * pitch + cx as usize * 4),
                    cw as usize * 4,
                )
            };
            let chunks = row.as_chunks::<4>().0;
            for sx in (0..cw as usize).step_by(x_step) {
                let px = &chunks[sx];
                let i = base + xbin[sx];
                sums[i * 3] += px[2] as u32; // BGRA → R
                sums[i * 3 + 1] += px[1] as u32; // G
                sums[i * 3 + 2] += px[0] as u32; // B
                counts[i] += 1;
            }
        }
        let mut out = Vec::with_capacity(n * 4);
        for i in 0..n {
            let c = counts[i].max(1) as u32;
            out.push((sums[i * 3] / c) as u8);
            out.push((sums[i * 3 + 1] / c) as u8);
            out.push((sums[i * 3 + 2] / c) as u8);
            out.push(255);
        }
        out
    }

    /// 瓦片增量：确保整屏降采样底图尺寸为 tw×th，对脏瓦片降采样 patch，
    /// 返回自上次以来变化的瓦片 [(col, row, w, h, RGBA)]。
    /// staging 必须已被整屏拷贝（grab_new 瓦片分支保证）。
    fn tile_delta(&mut self, max_tw: u32) -> Option<TileDelta> {
        // 请求下一帧：本次读完释放锁后，总线会为下一次调用拷贝一帧。
        self.tile_want = true;
        // 本会话是否需全量重发（见 tiles_fresh 文档）。只读快照：全量 pass 未完成
        // 前保持置位，提前返回（如 copied 与期望不符）不影响后续重试。
        let fresh = self.tiles_fresh;
        let tw = max_tw.min(self.w).max(1);
        let th = ((self.h as u64 * tw as u64) / self.w as u64).max(1) as u32;
        if self.tiles.tw != tw || self.tiles.th != th {
            self.tiles.resize(tw, th);
        }
        // 当前脏瓦片对应的期望包围盒；无脏瓦片时 None（无变化）。
        let expect = self.dirty_src_bbox()?;
        // staging 只含「脏瓦片源包围盒」这一块（CopySubresourceRegion 拷到左上角）。
        // 若实际拷贝区域与期望不符（如刚开启瓦片模式、总线尚未跑），等下一帧。
        let (bx, by, bw, bh) = self.copied?;
        if (bx, by, bw, bh) != expect {
            return None;
        }
        let (cols, rows) = (self.tiles.cols, self.tiles.rows);
        if cols == 0 || rows == 0 {
            return None;
        }
        let tw_us = tw as usize;
        let mut changed: Vec<TileUpdate> = Vec::new();
        let base_ptr_out = self.tiles.base.as_mut_ptr();
        // 只清「本帧实际处理」的瓦片；落在 bbox 外的保留到下一帧。
        let dirty = std::mem::take(&mut self.tiles.dirty);
        let mut remaining = vec![false; (cols * rows) as usize];
        let sums = &mut self.avg_sums;
        let counts = &mut self.avg_counts;
        // Map staging（RAII：异常展开也保证 Unmap）；失败时回填脏标记——
        // 否则本帧变化的瓦片永久丢失，底图停止更新且无恢复路径。
        let mapped = unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            if self
                .context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .is_err()
            {
                self.tiles.dirty = dirty;
                return None;
            }
            MappedStaging {
                context: self.context.clone(),
                texture: self.staging.clone(),
                ptr: mapped.pData as *const u8,
                pitch: mapped.RowPitch as usize,
            }
        };
        let (src, pitch) = (mapped.ptr, mapped.pitch);
        unsafe {
            for r in 0..rows {
                for c in 0..cols {
                    let idx = (r * cols + c) as usize;
                    if !dirty.get(idx).copied().unwrap_or(false) {
                        continue;
                    }
                    // 瓦片在降采样图中的像素范围
                    let dx0 = c * TILE_PX;
                    let dy0 = r * TILE_PX;
                    let dw = (tw - dx0).min(TILE_PX);
                    let dh = (th - dy0).min(TILE_PX);
                    // 对应源图范围（按比例回映射）
                    let sx0 = (dx0 as u64 * self.w as u64 / tw as u64) as u32;
                    let sy0 = (dy0 as u64 * self.h as u64 / th as u64) as u32;
                    let sx1 =
                        ((dx0 + dw) as u64 * self.w as u64 / tw as u64).min(self.w as u64) as u32;
                    let sy1 =
                        ((dy0 + dh) as u64 * self.h as u64 / th as u64).min(self.h as u64) as u32;
                    // 瓦片源矩形必须完整落在已拷贝的 bbox 内，否则留到下一帧。
                    if sx0 < bx || sy0 < by || sx1 > bx + bw || sy1 > by + bh {
                        remaining[idx] = true;
                        continue;
                    }
                    let sw = sx1.saturating_sub(sx0).max(1);
                    let sh = sy1.saturating_sub(sy0).max(1);
                    // staging 内偏移 = 源坐标 - bbox 起点。
                    let stx = (sx0 - bx) as usize;
                    let sty = (sy0 - by) as usize;
                    // 面积平均降采样该瓦片（复用累加缓冲）
                    let n = (dw * dh) as usize;
                    sums.clear();
                    sums.resize(n * 3, 0);
                    counts.clear();
                    counts.resize(n, 0);
                    let xbin: Vec<usize> = (0..sw as usize)
                        .map(|i| i * dw as usize / sw as usize)
                        .collect();
                    for yy in 0..sh as usize {
                        let yb = yy * dh as usize / sh as usize;
                        let rowp = std::slice::from_raw_parts(
                            src.add((sty + yy) * pitch + stx * 4),
                            sw as usize * 4,
                        );
                        let base = yb * dw as usize;
                        for (xx, px) in rowp.as_chunks::<4>().0.iter().enumerate() {
                            let i = base + xbin[xx];
                            sums[i * 3] += px[2] as u32;
                            sums[i * 3 + 1] += px[1] as u32;
                            sums[i * 3 + 2] += px[0] as u32;
                            counts[i] += 1;
                        }
                    }
                    // 降采样结果与持久底图中已有像素对比：完全相同则视为未变，
                    // 不重绘、不传输（DXGI 脏矩形常过度报告，这一步消除无效增量）。
                    // 全量重发会话（fresh）跳过该比对：前端画布是新建的，未变化
                    // 瓦片也必须发送，否则该区域无内容可绘（黑块）。
                    let mut tile_rgba = Vec::with_capacity(n * 4);
                    let mut same = !fresh;
                    for yy in 0..dh as usize {
                        for xx in 0..dw as usize {
                            let i = yy * dw as usize + xx;
                            let cnt = counts[i].max(1) as u32;
                            let rr = (sums[i * 3] / cnt) as u8;
                            let gg = (sums[i * 3 + 1] / cnt) as u8;
                            let bb = (sums[i * 3 + 2] / cnt) as u8;
                            let off = (((dy0 + yy as u32) * tw_us as u32 + (dx0 + xx as u32)) * 4)
                                as isize;
                            if same
                                && (*base_ptr_out.offset(off) != rr
                                    || *base_ptr_out.offset(off + 1) != gg
                                    || *base_ptr_out.offset(off + 2) != bb)
                            {
                                same = false;
                            }
                            tile_rgba.push(rr);
                            tile_rgba.push(gg);
                            tile_rgba.push(bb);
                            tile_rgba.push(255);
                        }
                    }
                    if same {
                        // 内容未变：不 patch、不发送。
                        continue;
                    }
                    // 写入持久底图对应位置
                    let mut o = 0usize;
                    for yy in 0..dh as usize {
                        for xx in 0..dw as usize {
                            let off = (((dy0 + yy as u32) * tw_us as u32 + (dx0 + xx as u32)) * 4)
                                as isize;
                            *base_ptr_out.offset(off) = tile_rgba[o];
                            *base_ptr_out.offset(off + 1) = tile_rgba[o + 1];
                            *base_ptr_out.offset(off + 2) = tile_rgba[o + 2];
                            *base_ptr_out.offset(off + 3) = 255;
                            o += 4;
                        }
                    }
                    changed.push((c, r, dw, dh, tile_rgba));
                }
            }
        }
        drop(mapped);
        // 只保留「落到 bbox 外、待下一帧处理」的脏标记；其余清除。
        self.tiles.dirty = remaining;
        // 全量 pass 完成（无瓦片留待下一帧）后复位：只需保证每次会话开启后的
        // 首个响应铺满全部瓦片。
        if fresh && !self.tiles.dirty.iter().any(|d| *d) {
            self.tiles_fresh = false;
        }
        if changed.is_empty() {
            return None;
        }
        Some((tw, th, changed))
    }
}

struct Entry {
    dup: Mutex<MonitorDup>,
    /// 最近使用时刻（UNIX 秒），LRU 淘汰用。
    accessed: std::sync::atomic::AtomicU64,
}

struct Shared {
    monitors: HashMap<MonitorKey, Arc<Entry>>,
    bus_started: bool,
}

fn shared() -> &'static Mutex<Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Shared {
            monitors: HashMap::new(),
            bus_started: false,
        })
    })
}

/// 取（或创建）某显示器的复制会话；首次调用会启动帧总线线程。
fn get_or_create(key: MonitorKey) -> Result<Arc<Entry>, Unavailable> {
    let now_secs = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    };

    // 快路径：短锁内查已有会话（顺带惰性启动帧总线）。
    {
        let s = shared();
        let mut s = s.lock().unwrap_or_else(PoisonError::into_inner);
        if !s.bus_started {
            s.bus_started = true;
            let _ = std::thread::Builder::new()
                .name("game-scanner-dup-bus".into())
                .spawn(bus_loop);
        }
        if let Some(e) = s.monitors.get_mut(&key) {
            e.accessed
                .store(now_secs(), std::sync::atomic::Ordering::Relaxed);
            return Ok(Arc::clone(e));
        }
    }

    // 慢路径：**锁外**创建会话（D3D11 设备创建数十~数百 ms，不能占用全局锁，
    // 否则会阻塞总线快照、其它显示器建会话与 clear_*_roi）。
    let created = create_monitor(key).map_err(|_| Unavailable)?;

    // 再抢锁插入；并发下可能已被他人创建，则复用已存在的（丢弃本次创建）。
    let s = shared();
    let mut s = s.lock().unwrap_or_else(PoisonError::into_inner);
    let now = now_secs();
    if let Some(e) = s.monitors.get_mut(&key) {
        e.accessed.store(now, std::sync::atomic::Ordering::Relaxed);
        return Ok(Arc::clone(e));
    }
    let entry = Arc::new(Entry {
        dup: Mutex::new(created),
        accessed: std::sync::atomic::AtomicU64::new(now),
    });
    // 容量上限：超出时淘汰最久未用的（显示器热拔插后旧条目自然清理）。
    if s.monitors.len() >= 5 {
        if let Some(oldest) = s
            .monitors
            .iter()
            .min_by_key(|(_, e)| e.accessed.load(std::sync::atomic::Ordering::Relaxed))
            .map(|(k, _)| *k)
        {
            s.monitors.remove(&oldest);
        }
    }
    s.monitors.insert(key, Arc::clone(&entry));
    Ok(entry)
}

/// 帧总线线程：60Hz 轮询每块显示器的复制会话。
fn bus_loop() {
    loop {
        std::thread::sleep(Duration::from_millis(15));
        let entries: Vec<(MonitorKey, Arc<Entry>)> = {
            let s = shared();
            let s = s.lock().unwrap_or_else(PoisonError::into_inner);
            s.monitors
                .iter()
                .map(|(k, v)| (*k, Arc::clone(v)))
                .collect()
        };
        for (key, entry) in entries {
            // try_lock：消费者正在 Map（如 4K 整帧转换）时跳过本轮，下轮补上；
            // 锁被毒化（消费者 panic）时取回内部值继续，否则该显示器总线永久停摆。
            let mut m = match entry.dup.try_lock() {
                Ok(g) => g,
                Err(std::sync::TryLockError::WouldBlock) => continue,
                Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| m.grab_new()));
            match r.unwrap_or(Grab::Broken) {
                Grab::New => m.frame_id += 1,
                Grab::NoChange => {}
                Grab::Broken => {
                    drop(m);
                    let s = shared();
                    let mut s = s.lock().unwrap_or_else(PoisonError::into_inner);
                    s.monitors.remove(&key);
                }
            }
        }
    }
}

/// 为显示器创建复制会话（D3D11 设备 + staging 纹理 + 首帧采集）。
fn create_monitor(key: MonitorKey) -> Result<MonitorDup, ()> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|_| ())?;
        let center = (key.0 + key.2 / 2, key.1 + key.3 / 2);
        let mut found: Option<(IDXGIOutput, DXGI_OUTPUT_DESC, IDXGIAdapter1)> = None;
        for ai in 0u32.. {
            let Ok(adapter) = factory.EnumAdapters1(ai) else {
                break;
            };
            for oi in 0.. {
                let Ok(output) = adapter.EnumOutputs(oi) else {
                    break;
                };
                let Ok(desc) = output.GetDesc() else {
                    continue;
                };
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let rc = &desc.DesktopCoordinates;
                if center.0 >= rc.left
                    && center.0 < rc.right
                    && center.1 >= rc.top
                    && center.1 < rc.bottom
                {
                    found = Some((output, desc, adapter));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (output, odesc, adapter) = found.ok_or(())?;
        let adapter_base: IDXGIAdapter = adapter.cast().map_err(|_| ())?;
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            &adapter_base,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(D3D11_CREATE_DEVICE_BGRA_SUPPORT.0),
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|_| ())?;
        let device = device.ok_or(())?;
        let context = context.ok_or(())?;
        let output1: IDXGIOutput1 = output.cast().map_err(|_| ())?;
        let dup = output1.DuplicateOutput(&device).map_err(|_| ())?;
        let dd = dup.GetDesc();
        // 仅支持 SDR BGRA；HDR（浮点格式）走 GDI 兜底。
        if dd.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || dd.ModeDesc.Width == 0
            || dd.ModeDesc.Height == 0
        {
            return Err(());
        }
        let (w, h) = (dd.ModeDesc.Width, dd.ModeDesc.Height);
        let sdesc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        device
            .CreateTexture2D(&sdesc, None, Some(&mut staging))
            .map_err(|_| ())?;
        let staging = staging.ok_or(())?;
        let rc = &odesc.DesktopCoordinates;
        let output_rect = (rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top);
        let mut m = MonitorDup {
            output_rect,
            context,
            dup,
            staging,
            w,
            h,
            frame_id: 0,
            scan_seen: [0; 3],
            prev_seen: 0,
            prev_tw: 0,
            prev_region: None,
            prev_hash: 0,
            preview_roi: None,
            scan_roi: None,
            roi_dirty: false,
            copied: None,
            avg_sums: Vec::new(),
            avg_counts: Vec::new(),
            tiles: TileState::new(),
            tile_mode: false,
            tile_want: false,
            tiles_fresh: false,
        };
        if matches!(m.grab_new(), Grab::New) {
            m.frame_id = 1;
        }
        Ok(m)
    }
}

// ---------- 公开 API ----------

/// 桌面复制不可用（安全桌面 / 不支持的格式 / 设备丢失）。
/// 调用方应回退 GDI 截屏。
#[derive(Debug)]
pub struct Unavailable;

/// 预览取帧结果。
pub enum PreviewOut {
    /// 新帧（RGBA + 宽 + 高）
    Frame(Vec<u8>, u32, u32),
    /// 帧未变化（消费者可沿用上一帧）
    Unchanged,
}

/// 找到包含 rect 中心的显示器矩形（缓存键必须是显示器，而非任意区域——
/// 否则平移/缩放导致区域变化时会反复创建/销毁复制会话）。
fn monitor_key_for(rect: MonitorKey) -> Option<MonitorKey> {
    let (cx, cy) = (rect.0 + rect.2 / 2, rect.1 + rect.3 / 2);
    crate::screenshot::screen_rects()
        .into_iter()
        .find(|m| cx >= m.0 && cx < m.0 + m.2 && cy >= m.1 && cy < m.1 + m.3)
}

/// 预览取帧：region 为可视视口（显示器物理坐标），降采样到 tw 宽。
/// Err = 桌面复制不可用（调用方走 GDI 兜底）。
pub fn preview_frame(region: MonitorKey, tw: u32) -> Result<PreviewOut, Unavailable> {
    let mkey = monitor_key_for(region).ok_or(Unavailable)?;
    let entry = get_or_create(mkey)?;
    let mut m: MutexGuard<MonitorDup> = entry.dup.lock().unwrap_or_else(PoisonError::into_inner);
    // 常规预览请求到达 ⇒ 前端已回到普通预览（瓦片底图只在拖拽/缩放进行中使用）。
    // 若此处仍残留 tile_mode，帧总线只拷贝「脏瓦片包围盒」而不覆盖完整请求区域，
    // staged_crop 因此恒为 None → 前端收到「未变化」→ 画面长期冻结（实测：滚轮缩放
    // 到极限后残留瓦片模式，预览停更，需再次点击/拖拽才恢复）。此处兜底关闭：
    // 下一次 capture_tiles 会自行重新开启（tile_frame 的兜底分支），不影响拖拽。
    if m.tile_mode {
        m.tile_mode = false;
        m.roi_dirty = true;
    }
    match m.preview_frame(region, tw).map_err(|_| Unavailable)? {
        Some((rgba, w, h)) => Ok(PreviewOut::Frame(rgba, w, h)),
        None => Ok(PreviewOut::Unchanged),
    }
}

/// 切换瓦片（拖拽底图）模式。开启后帧总线只拷贝脏瓦片源包围盒并标记脏瓦片。
pub fn set_tile_mode(region: MonitorKey, on: bool) {
    let Some(mkey) = monitor_key_for(region) else {
        return;
    };
    let Ok(entry) = get_or_create(mkey) else {
        return;
    };
    let mut m = entry.dup.lock().unwrap_or_else(PoisonError::into_inner);
    if m.tile_mode != on {
        m.tile_mode = on;
        m.roi_dirty = true;
        if on {
            // 每次开启（新一次拖拽）都强制全量：前端起手会重建底图 canvas，
            // 需一次性铺满全部瓦片，否则只收到零星脏瓦片 → 大面积黑屏。
            // 注意：仅「标脏」不够——tile_delta 会对未变化瓦片做内容比对并跳过
            // 发送，而 tiles.base 跨会话保留；必须同时置 tiles_fresh 无条件重发
            //（每次拖拽起手的黑块根因）。
            m.tiles.dirty.iter_mut().for_each(|d| *d = true);
            m.tiles_fresh = true;
            m.tile_want = true; // 请求首帧
        }
    }
}

/// 瓦片增量取帧：返回 (降采样宽, 高, [(col,row,w,h,RGBA)])。
/// 无变化返回 None；桌面复制不可用返回 Err。
pub fn tile_frame(region: MonitorKey, max_tw: u32) -> Result<Option<TileDelta>, Unavailable> {
    let mkey = monitor_key_for(region).ok_or(Unavailable)?;
    let entry = get_or_create(mkey)?;
    let mut m: MutexGuard<MonitorDup> = entry.dup.lock().unwrap_or_else(PoisonError::into_inner);
    if !m.tile_mode {
        // 兜底路径（前端未先调 set_tile_mode）：与开启语义一致——标脏全部瓦片
        // 并要求全量重发，保证首个响应铺满。
        m.tile_mode = true;
        m.roi_dirty = true;
        m.tile_want = true;
        m.tiles.dirty.iter_mut().for_each(|d| *d = true);
        m.tiles_fresh = true;
    }
    Ok(m.tile_delta(max_tw))
}

/// 清除预览关注区（预览暂停时调用）：暂停后不再请求预览，但 preview_roi 仍残留，
/// 帧总线会继续因桌面变化做拷贝 → GPU 占用居高不下。清除它让总线跳过拷贝。
pub fn clear_preview_roi() {
    let s = shared();
    let s = s.lock().unwrap_or_else(PoisonError::into_inner);
    for e in s.monitors.values() {
        let mut m = e.dup.lock().unwrap_or_else(PoisonError::into_inner);
        if m.preview_roi.is_some() {
            m.preview_roi = None;
            m.roi_dirty = true;
        }
    }
}

/// 清除扫码关注区（扫描结束后调用）：避免残留 ROI 令脏矩形过滤过于保守
/// （放大预览时把已停止的扫码区域也算进关注区）。
pub fn clear_scan_roi(region: MonitorKey) {
    let Some(mkey) = monitor_key_for(region) else {
        return;
    };
    let s = shared();
    let s = s.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(e) = s.monitors.get(&mkey) {
        let mut m = e.dup.lock().unwrap_or_else(PoisonError::into_inner);
        if m.scan_roi.is_some() {
            m.scan_roi = None;
            m.roi_dirty = true;
        }
    }
}

/// 扫码取帧：区域画面自上次调用以来有变化时返回灰度帧；无变化返回 None。
/// `scale` 为步长降采样倍率（1 = 原分辨率，2 = 1/2；转换与降采样合并为一趟）。
/// Err = 桌面复制不可用（调用方走 GDI 兜底）。
pub fn scan_frame(
    region: MonitorKey,
    scale: u32,
) -> Result<Option<crate::screenshot::GrayFrame>, Unavailable> {
    let mkey = monitor_key_for(region).ok_or(Unavailable)?;
    let entry = get_or_create(mkey)?;
    let mut m: MutexGuard<MonitorDup> = entry.dup.lock().unwrap_or_else(PoisonError::into_inner);
    m.scan_frame(region, scale).map_err(|_| Unavailable)
}
