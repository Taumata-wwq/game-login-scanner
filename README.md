# 游戏扫码器

Windows 桌面 + Android 双端工具：游戏登录界面出现二维码时，用已登录的账号**自动完成扫码确认**，不用手机扫码、不用输密码。

Tauri 2（Rust + 系统 WebView）构建。桌面端单文件可执行（约 4.9 MB）；Android 端 APK 约 35 MB（含渠道 SDK）。

---

## 功能

- **扫码登录**：读剪贴板 → 扫屏幕 → 识别后自动确认（2 分钟内上限 10 次，服务端限流保护）。
- **直播抢码**：保存直播间列表，直接拉流解码抢码。B 站为弹幕长连接（开播/下播秒级推送），抖音为 30 秒轮询；端到端延迟约 0.3~0.4 秒。
- **多路并行**：桌面 + 最多 5 个直播间可同时扫描，各自独立启停。
- **Android 三路扫码**：屏幕扫码（MediaProjection）、摄像头扫码、直播抢码；均在本机完成，不需要投屏。
- **账号体系**：同渠道多账号、备注、拖拽排序；令牌加密落盘（桌面 DPAPI / Android Keystore AES-256-GCM）。
- **设置**：启动自动扫码、窗口置顶、扫码成功自动退出/停止其他直播间、通知（移动端）。

## 支持的游戏与渠道

| 厂商 | 游戏 | 渠道 |
| --- | --- | --- |
| 米哈游 | 崩坏3 | 官服、B 服、小米、vivo、应用宝(QQ)、Wegame(QQ)、华为、UC(九游)、OPPO |
| 米哈游 | 崩坏：星穹铁道 / 原神 / 绝区零 | 官服、B 服 |
| 鹰角 | 明日方舟 / 明日方舟：终末地 | 鹰角账号 |

**验证分级**（避免「写了就是支持」）：

| 渠道 | 状态 |
| --- | --- |
| 官服（米游社）、B 服 | 真机 + 桌面实测登录通过 |
| 华为 | 真机实测通过（需自备 HMS/AGC 凭据） |
| 小米、vivo、应用宝、Wegame | 已实装，登录界面可拉起；combo 兑换全链路待渠道账号验证 |
| UC(九游) | 已实装，真机已拉起 UC 登录界面；待九游账号 |
| OPPO | 已实装，真机已拉起 SDK；**需安装「移动服务框架」(com.heytap.htms)**，非 OPPO 设备无法完成登录 |

> 第三方渠道的登录令牌由厂商 SDK 在本机签发，不经过任何第三方服务器。

## 使用

**桌面端**

1. 选游戏 → 「添加账号」选渠道与登录方式（二维码 / 短信 / 密码；触发极验时弹独立验证窗）。
2. 游戏停在登录二维码界面 → 「开始扫码」。
3. 侧栏添加直播间（B站/抖音），点击卡片即切换；拖拽平移、滚轮缩放。

**Android 端**

- 底部三键：屏幕扫码（授权后自动切后台去扫游戏，切回自动停止）/ 开始扫码（对当前目标启停）/ 摄像头扫码。
- 左右抽屉：左为直播间，右为设置与日志（含屏幕共享状态与「停止共享」）。
- 返回键：取景/抽屉/弹窗先做界面复位；主界面按返回退到桌面并保持后台。

## 数据位置与安全

```
Windows: %APPDATA%\游戏扫码器\config.json（账号与设置）
         %APPDATA%\游戏扫码器\game-login-scanner.log（超 2 MB 轮转 .old）
Android: 应用私有目录；日志 adb logcat -s GameScanner
```

不保存账号密码，仅保存登录令牌；桌面用 DPAPI、Android 用 Keystore 加密。

## 构建

**桌面（Windows 10 1803+，需 WebView2）**

```powershell
cd src-tauri
cargo run                 # 调试
cargo build --release     # 产物 target/release/game-login-scanner.exe
```

**Android**（JDK 21 + Android SDK 36 + NDK 27 + Rust Android 目标）

```powershell
pwsh -File scripts/android/build-android.ps1 -CopyToDist
```

该脚本是**唯一**构建入口：先跑 `patch-gen.ps1` 重放 `gen/` 的本地化补丁（Kotlin 插件、渠道 SDK、清单声明、图标、混淆规则、ABI 裁剪），再调 `cargo tauri android build`；末尾依次执行渠道依赖检查、JNI 契约检查、APK 存在性门禁。

**图标**：源文件 `icon2.png`，由脚本生成 `src-tauri/icons/`（桌面）与 `src-tauri/android-icons/`（Android 5 个密度档），构建时由 patch-gen 同步进 `gen/`。

**质量门（本地与 CI 一致）**

```powershell
cd src-tauri
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                 # 114 passed
```

CI 另含：Android 构建脚本语法检查、Kotlin↔Rust JNI 契约检查。

## 目录结构

```
src/                      前端（无构建链，直接 <script src>）
  index.html / app.css / app.js / dev-preview.js（浏览器预览 mock）
src-tauri/src/            后端
  lib.rs                  Tauri 命令接线与运行时状态
  live.rs                 直播拉流 / 解码 / 追帧 / 帧总线（FLV + HLS）
  scan.rs                 扫码主流程（多目标并行）
  duplication.rs          DXGI 桌面复制（Windows）
  android/                Android 侧桥接（MediaProjection / MediaCodec / Keystore）
  sdk/                    各渠道登录协议（官方 / B站 / 米哈游 / 渠道 / 鹰角）
src-tauri/android-plugin/ Kotlin 插件（渠道登录、屏幕捕获、通知、剪贴板…）
scripts/android/          patch-gen / build-android / 依赖与 JNI 契约检查
vendor/                   渠道 SDK 二进制（不入库）
```

## 文档

设计决策、渠道核对、直播链路排查、遗留问题与路线图集中在文档仓库：
**https://github.com/Taumata-wwq/dev-docs**（私有，按项目分目录）

## 常见问题

- **直播画面帧率**：显示固定 30fps（帧率设置项已移除）；扫码捕获不受影响，仍按源帧率/最高频率取帧。
- **OPPO 渠道**：需先在设备上安装「移动服务框架」，否则 SDK 只弹安装提示。
- **B 服 / 原神等**：B 服仅崩坏3 实测通过，其余米哈游游戏同源但未实测。
- **Windows N/K 版**：需安装「媒体功能包」，否则 H.264 解码不可用（应用会给出明确提示）。

## 许可与免责

主体 MIT（见 `LICENSE`），第三方组件与系统组件见 `THIRD-PARTY-NOTICES.md`。

本项目仅用于**自己的账号**在自己设备上的登录便利；请遵守各游戏/平台的服务条款。因使用不当造成的账号风险由使用者承担。
