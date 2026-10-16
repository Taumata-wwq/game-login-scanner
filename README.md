# 游戏扫码器

Windows 桌面 + Android 双端工具。游戏登录界面出现二维码时，用已登录的账号自动完成扫码确认，无需使用手机扫码或手动输入账号。

基于 Tauri 2（Rust + 系统 WebView）构建：桌面端为单文件可执行程序（体积约 4.5 MB），Android 端为 APK（按 ABI 裁剪，16–24 MB；支持屏幕扫码、摄像头扫码与直播抢码）。

支持的游戏：

| 厂商 | 游戏 | 登录渠道 |
| --- | --- | --- |
| 米哈游 | 崩坏3 / 崩坏：星穹铁道 / 原神 / 绝区零 | 官服（米游社）/ B 服（Bilibili） |
| 鹰角 | 明日方舟：终末地 / 明日方舟 | 鹰角账号 |

同一米游社 / B 站账号可登录米哈游全系游戏。账号按游戏独立存储与选择。

> **渠道测试状态**：B 服目前仅在 **崩坏3** 上实测通过；**原神 / 崩坏：星穹铁道 / 绝区零** 的 B 服渠道**尚未测试**（与崩坏3同源、预期可用，但不保证，欢迎反馈）。官服（米游社）渠道四款均已实测。

> **Android 端测试状态**：屏幕扫码、摄像头扫码（官服实测登录通过）、直播抢码（B站 / 抖音）已在真机验证（Redmi K40 / 安卓 13、华为 / 安卓 9）；剪贴板读图与第三方渠道暂未实现。

## 使用

1. **选择游戏**：点击标题栏游戏名，按厂商分组选择目标游戏。
2. **登录账号**：点击「添加账号」，选择渠道（官服 / B 服 / 鹰角）与登录方式（二维码 / 短信验证码 / 密码）。登录成功后账号自动保存并设为当前账号，同渠道支持多账号。
   - 触发人机验证（极验）时弹出独立验证窗口，完成后自动继续。
3. **管理账号**：点击账号行切换为当前账号；悬停行末尾「✕」删除；双击行编辑备注名与备注信息；拖拽调整顺序；列表底部虚线按钮添加新账号。
4. **扫码进游戏**：游戏停在登录界面出现二维码后，点击「开始扫码」。工具先读取剪贴板，未命中则搜索屏幕，识别后用当前账号自动确认。2 分钟内成功上限 10 次（服务端限流保护）。
5. **直播抢码**：侧栏保存直播间列表（B站 / 抖音）。点击卡片立即切换——已连接的直接显示画面；未连接的先显示封面占位，连线成功后自动揭除；未开播的也可进入（显示封面），开播后自动连接。鼠标拖拽平移、滚轮缩放，画面平滑跟手。B站直播间有弹幕长连接，开播 / 下播**秒级推送**；抖音为 30 秒轮询级。
6. **多路并行扫码**：桌面 + 最多 5 个直播间可**同时扫描**，各自独立启停（右上角按钮随当前视图切换目标）；正在扫描的卡片显示「识别中」，确认成功显示「已确认」。
7. **设置与日志**：点击标题栏图标打开弹窗，包含登入名、启动时自动开启扫码、窗口置顶、扫码成功后自动退出、扫码成功后停止其他直播间扫码、显示区域帧率（30 / 60 / 120），下方为运行日志（可清空）。

窗口位置 / 尺寸、侧栏与游戏列宽、最后观看的直播间、账号与游戏选择、全部设置项均自动记忆，重启后原样恢复。应用为单实例运行（重复启动自动退出，防止配置互相覆盖）。

## Android 端

与桌面端同仓库构建（Tauri 2 Mobile），移动端界面为触屏优化版：

- 底部操作栏三键：**屏幕扫码**（先授权，随后自动把应用切到后台去扫游戏；切回应用自动停止）、**开始扫码**（对当前目标启停）、**摄像头扫码**（取景模式——直接把取景框对准另一块屏幕上的登录二维码即可；识别成功后自动退回主界面）。
- 左右抽屉：左为直播间列表，右为设置与运行日志。
- 屏幕 / 摄像头扫码均在本机完成，不需要投屏或其它设备。
- 构建：先 `pwsh -File scripts/android/patch-gen.ps1`（生成目录补丁，幂等），再 `cargo tauri android build`；依赖 JDK、Android SDK/NDK 与 Rust Android 目标。

## 延迟说明

直播抢码直接拉取直播流边下边解（B站主流 FLV、HLS-only 房间自动回退 HLS；抖音 FLV），不经过浏览器播放链路，延迟约 0.3~0.4 秒。视频解码：桌面端走 Windows Media Foundation **H.264 硬解**（系统组件、DXVA 加速、零体积增量；不支持硬件解码的环境自动回退系统软解，画面与扫码不受影响；Windows N/K 版需安装「媒体功能包」），Android 端走 **MediaCodec 硬解**。若本机解码吞吐不足，自动逐级降低清晰度（原画 → 高清 → 标清 → 流畅）；扫码对清晰度要求不高。CDN 节点异常时自动切换节点重连。支持 B站 与 抖音 直播间。

## 数据位置

```
桌面端：
%APPDATA%\游戏扫码器\config.json               配置与账号
%APPDATA%\游戏扫码器\game-login-scanner.log    运行日志（超 2 MB 自动轮转为 .old）
Android 端：
应用私有目录 /data/data/com.taumata.game_scanner/（不可直接访问）
日志经 logcat 查看：adb logcat -s GameScanner
```

工具不输入、不存储账号密码，仅保存登录令牌：桌面端使用 Windows DPAPI 加密落盘（绑定当前用户，换机器或换用户无法解密），Android 端使用 Android Keystore（AES-256-GCM，密钥不出安全硬件）。

## 构建

要求 **Windows 10 1803 及以上**（DXGI 桌面复制 `IDXGIOutputDuplication` 的最低版本；
需 WebView2 运行时，Win11 已内置，缺失时安装包会联网下载）与 [Rust](https://rustup.rs/)：

```powershell
cd src-tauri
cargo run                 # 调试运行
cargo build --release     # 产物：target\release\game-login-scanner.exe（约 4.5 MB）
```

代码结构：

```
src/index.html          主界面（原生 HTML/CSS/JS，无前端构建链）
src/verify.html         人机验证窗口
src-tauri/src/
  lib.rs                Tauri 命令接线与运行时状态
  scan.rs               扫码主流程（多目标并行：桌面 / 各直播会话；剪贴板 → 帧 → 确认登录）
  live.rs               直播流捕获（接口解析 → FLV/HLS 解复用 → 解码管线；多会话与扫码保活）
  watch.rs              B站直播间开播/下播弹幕长连接（秒级推送）
  decode.rs             解码器抽象（Media Foundation H.264 硬解）
  mf_decode.rs          Media Foundation H.264 硬解（MFT 封装，NV12→I420）
  hls.rs                HLS（LL-HLS + fMP4）解析（HLS-only 房间回退路径）
  qr.rs                 二维码识别（rxing，ZXing 纯 Rust 移植）与生成（qrcodegen）
  screenshot.rs         GDI 截屏（兜底路径）
  duplication.rs        DXGI 桌面复制（低开销取帧）
  clipboard.rs          剪贴板读图
  sdk/                  各渠道登录协议（mihoyo / official / bili / hypergryph / cred_login）
  android/              Android 适配层（MediaProjection / Camera2 / MediaCodec / Keystore 桥）
  config.rs / crypto.rs / http.rs / constants.rs / douyin.rs
src-tauri/android-plugin/   Android 插件层 Kotlin 源码（屏幕捕获 / 摄像头 / 系统 TLS 桥）
```

> 单元测试、真实网络探针与解码基准等内部验证设施为本地材料，不随仓库分发。

## 注意事项

- 仅用于登录本人游戏账号，请勿用于非授权用途。
- 「操作过于频繁」等提示为服务端正常限流，稍后重试即可。
- 二维码识别受画面清晰度影响，请避免二维码被遮挡或模糊。

## 参考

### 接口与协议

- [bilibili-API-collect](https://github.com/SocialSisterYi/bilibili-API-collect)：B站 API 文档（直播间信息 / 直播流解析 / wbi 签名 / 弹幕长连接 / 电视端扫码登录的依据）
- [mihoyo-api-collect](https://github.com/UIGF-org/mihoyo-api-collect)：米哈游 API 文档（二维码 / 密码 / 短信验证码登录协议依据）
- [MHY_Scanner](https://github.com/DSVVA/MHY_Scanner)：同类工具（协议与实现思路参考）
- [BBH3ScanLaunch](https://github.com/LoveElysia1314/BBH3ScanLaunch)：社区项目（B 服游戏版本与 OA 分发串基线来源）
- [hellocraft scanner](https://api-v2.scanner.hellocraft.xyz/v4/hi3_version)：社区维护的 B 服接口服务（版本查询 `v4/hi3_version` 与 OA 分发 `v3/query_dispatch`，运行时按当前游戏版本取分发串）
- 抖音直播：站内接口，无公开文档（依据实测与站内结构解析）

### 框架与系统能力

- [Tauri](https://tauri.app/)：应用框架（Rust + 系统 WebView2，单文件发布）
- [H.264 Video Decoder · Media Foundation](https://learn.microsoft.com/windows/win32/medfound/h-264-video-decoder)：系统硬件解码器（直播 DXVA 硬解依据）
- [Desktop Duplication API](https://learn.microsoft.com/windows/win32/direct3ddxgi/desktop-dup-api)：屏幕取帧
- [DPAPI · CryptProtectData](https://learn.microsoft.com/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)：令牌加密落盘（绑定当前 Windows 用户）
- [RFC 8216 · HTTP Live Streaming](https://datatracker.ietf.org/doc/html/rfc8216)：HLS / LL-HLS 与 fMP4 分片解析依据
- [极验 Geetest](https://docs.geetest.com/)：人机验证（触发时弹出的独立验证窗口）

### 开源组件

- [rxing](https://github.com/rxing-core/rxing)：二维码/条码解码（ZXing 的纯 Rust 移植，Apache-2.0）
- [qrcodegen](https://github.com/nayuki/QR-Code-generator)：二维码生成（Nayuki，MIT）
- [NSIS](https://nsis.sourceforge.io/)：安装包打包（由 Tauri bundler 生成）

## 许可证

本项目以 [MIT](LICENSE) 许可发布，全部依赖均为宽松许可（MIT / Apache-2.0）——
商用分发前请阅读 [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md)。
