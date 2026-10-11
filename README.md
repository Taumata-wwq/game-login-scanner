# 游戏扫码器

Windows 桌面工具。游戏登录界面出现二维码时，用已登录的账号自动完成扫码确认，无需使用手机扫码或手动输入账号。

支持的游戏：

| 厂商 | 游戏 | 登录渠道 |
| --- | --- | --- |
| 米哈游 | 崩坏3 / 崩坏：星穹铁道 / 原神 / 绝区零 | 官服（米游社）/ B 服（Bilibili） |
| 鹰角 | 明日方舟：终末地 / 明日方舟 | 鹰角账号 |

同一米游社 / B 站账号可登录米哈游全系游戏。账号按游戏独立存储与选择。

## 使用

1. **选择游戏**：点击标题栏游戏名，按厂商分组选择目标游戏。
2. **登录账号**：点击「添加账号」，选择渠道（官服 / B 服 / 鹰角）与登录方式（二维码 / 短信验证码 / 密码）。登录成功后账号自动保存并设为当前账号，同渠道支持多账号。
   - 触发人机验证（极验）时弹出独立验证窗口，完成后自动继续。
3. **管理账号**：点击账号行切换为当前账号；悬停行末尾「✕」删除；双击行编辑备注名与备注信息；拖拽调整顺序；列表底部虚线按钮添加新账号。
4. **扫码进游戏**：游戏停在登录界面出现二维码后，点击「开始扫码」。工具先读取剪贴板，未命中则搜索屏幕，识别后用当前账号自动确认。2 分钟内成功上限 10 次（服务端限流保护）。
5. **直播抢码**：在账号区右侧输入框填入 B站直播间号，预览与扫码自动切换至该直播画面；清空输入框恢复屏幕模式。支持滚轮缩放与拖拽平移，窗口可视区域即扫描范围语义相同。未开播的直播间即时提示。
6. **设置与日志**：点击标题栏图标打开弹窗，包含登入名、启动时自动开启扫码、窗口置顶、扫码成功后自动退出、显示区域帧率（30 / 60 / 120），下方为运行日志（可清空）。

## 延迟说明

直播抢码直接拉取直播流边下边解（主流 FLV，HLS-only 房间自动回退 HLS），不经过浏览器播放链路，延迟约 0.4 秒。若本机解码吞吐不足，自动逐级降低清晰度（原画 → 高清 → 标清 → 流畅）；扫码对清晰度要求不高。CDN 节点异常时自动切换节点重连。当前仅支持 B站直播间。

## 数据位置

```
%APPDATA%\游戏扫码器\config.json               配置与账号
%APPDATA%\游戏扫码器\game-login-scanner.log    运行日志（超 2 MB 自动轮转为 .old）
```

工具不输入、不存储账号密码，仅保存登录令牌，使用 Windows DPAPI 加密落盘（绑定当前用户，换机器或换用户无法解密）。

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
  scan.rs               扫码主流程（剪贴板 → 屏幕/直播帧 → 确认登录）
  live.rs               直播流捕获（接口解析 → FLV/HLS 解复用 → openh264 解码）
  hls.rs                HLS（LL-HLS + fMP4）解析（HLS-only 房间回退路径）
  qr.rs                 二维码识别（zedbar）与生成（qrcodegen）
  screenshot.rs         GDI 截屏（兜底路径）
  duplication.rs        DXGI 桌面复制（低开销取帧）
  clipboard.rs          剪贴板读图
  sdk/                  各渠道登录协议（mihoyo / official / bili / hypergryph / cred_login）
  config.rs / crypto.rs / http.rs / constants.rs
```

测试：`cargo test`；真实网络探针（手动运行）：`cargo test --test probe -- --ignored --nocapture`

## 注意事项

- 仅用于登录本人游戏账号，请勿用于非授权用途。
- 「操作过于频繁」等提示为服务端正常限流，稍后重试即可。
- 二维码识别受画面清晰度影响，请避免二维码被遮挡或模糊。

## 参考

- [bilibili-API-collect](https://github.com/SocialSisterYi/bilibili-API-collect)
- [mihoyo-api-collect](https://github.com/UIGF-org/mihoyo-api-collect)
- [MHY_Scanner](https://github.com/DSVVA/MHY_Scanner)

## 许可证

主体以 MIT 发布（见 `LICENSE`）。二维码解码器 zedbar 为 LGPL-3.0-or-later，
商用分发前请阅读 `THIRD-PARTY-NOTICES.md`。
