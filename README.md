# 游戏扫码器

一个体积小巧、启动快速、界面无冗余的**多游戏登录二维码扫码器**，**仅支持 Windows 10+**。
界面为 Adobe / Clip Studio Paint 风格的专业黑白深色配色，无边框窗口 + 自绘标题栏，
固定 300px 宽（可上下调节高度，最小 400）。

## 支持的游戏

| 厂商 | 游戏 | 登录渠道 |
| --- | --- | --- |
| 米哈游 | 崩坏3 / 崩坏：星穹铁道 / 原神 / 绝区零 | 官服（米游社通行证）/ B 服（Bilibili） |
| 鹰角 | 明日方舟：终末地 / 明日方舟 | 鹰角账号 |

同一米游社 / B 站账号可登录米哈游全系游戏；账号按游戏独立存储与激活。

## 功能

- **多游戏切换**：标题栏「游戏名」按钮打开游戏选择弹窗（按厂商分组）。
  切换游戏只改变扫描确认时的 `biz_key` 与协议端点，账号可复用。
- **账号管理（按游戏隔离）**：筛选栏（官服 / B 服 / 鹰角）+「添加账号」弹窗；
  每个游戏可各激活一个账号（同渠道内单选），点击账号行切换当前使用账号。
  官服账号登录后显示米游社昵称。
- **扫码登录账号**（两种方式）：
  - **二维码**：官服码用**米游社 App**扫、B 服码用 **B 站 App**扫、鹰角码用**鹰角 App**扫。
  - **验证码 / 密码**：米哈游官服支持短信验证码与账号密码登录（含极验人机验证，
    在独立验证窗口中完成）。
- **扫码进游戏**：点击「开始扫码」后先读取一次剪贴板图像；未命中则高频截屏搜索登录二维码。
  识别到游戏登录码后用当前激活账号提交确认。
- **自定义扫描区域**：主界面内嵌**实时预览**（3:2 监视器，60fps），滚轮缩放、左键拖拽平移，
  **可视视口即扫描范围**；默认扫描全部显示器。
- **成功频率限制**：2 分钟内扫码成功上限 10 次，防滥用 / 风控。
- **CPU 优化**：DXGI 桌面复制（静止近零成本）、ROI 优先解码、2×2 降采样快路径、
  全分辨率解码节流、画面指纹去重、失败票据退避 + 日志去重。
- **无边框窗口**：自绘标题栏（拖拽 + 最小化 / 关闭），「选项与日志」为窗口内居中弹窗
  （含**启动时自动登录上次账号**、窗口置顶、成功后动作、检测间隔）。
- 主窗口关闭即退出，并**记住窗口位置**；**全程不接触、不存储账号密码**（仅保存登录令牌）。

## 技术栈

Rust + Tauri v2，前端为原生 HTML/CSS/JS（无 npm 依赖）。

| 模块 | 用途 |
| --- | --- |
| `rxing` | 二维码解码与登录二维码 SVG 生成 |
| `arboard` | 剪贴板读取 |
| `screenshots` | 全屏 / 区域截图（多显示器 + DPI） |
| `hmac` / `md-5` / `sha2` | 登录签名（HMAC-SHA256 / MD5） |
| `rsa` / `base64` | 账号密码 / 手机号 RSA 加密（PKCS1v15 + Base64） |
| `reqwest` | HTTP（进程级共享连接池） |
| `serde` + JSON | 数据持久化 |
| `windows` | 原生 Win32 API（DXGI 桌面复制） |

## 登录协议

- **B 服**：B 站 TV 扫码登录（`passport-tv-login/qrcode/auth_code` + `poll`，appkey 签名）
  → 取得 `access_token`（即游戏 SDK 的 access_key）与 mid。
- **官服**：米哈游通行证 App 扫码登录（`ma-cn-passport/app/createQRLogin` +
  `queryQRLoginStatus`，DSPROD 头）→ 取 tokens 中 token_type=1 的 stoken 与 uid / mid。
- **鹰角**：鹰角账号短信登录 + `scan_login` / `update_scan_status` 扫码确认。
- **米哈游验证码 / 密码登录**：通行证 Web 协议（`passport-api.mihoyo.com`）+
  极验 GT4 人机验证（独立窗口完成）；发短信走通行证 Web 协议、登录走 App 协议取 stoken。
- 米哈游系游戏码的**扫码确认共用** `api-sdk.mihoyo.com/{biz_key}/combo/panda/qrcode` 的
  `scan` + `confirm`：B 服走 HMAC-SHA256 签名，官服走 `game_token`（无需签名）。

## 环境要求

- Windows 10 及以上（需已安装 **WebView2 运行时**，Win11 已内置）。
- 开发构建需安装 **Rust 工具链**（[rustup](https://rustup.rs/)）。

## 构建与运行（开发者）

```powershell
cd src-tauri
cargo run                 # 调试运行
cargo build --release     # 发布构建，产物在 target\release\bh3-scanner.exe
cargo test                # 单元 / 集成测试（tests\ 目录）
```

> 注意：修改 `src/` 下的前端文件后，直接 `cargo build` 可能因增量编译不重新嵌入资源，
> 请先 `touch src/lib.rs`（或 `cargo clean -p bh3-scanner`）再构建。

## 使用说明

1. **选择游戏**：点击标题栏游戏名 → 选择目标游戏（按厂商分组）。
2. **登录账号**：点击「添加账号」，选择渠道与登录方式（二维码 / 验证码 / 密码）。
   同一渠道可登录多个账号，登录记录自动保存并设为当前激活账号。
3. **切换 / 删除账号**：点击账号行切换当前使用账号；悬停账号行点「✕」删除。
4. **扫码进游戏**：将游戏停在登录界面（屏幕出现登录二维码），点击「开始扫码」：
   - 先读剪贴板中的二维码图片，未命中则自动搜索登录二维码；
   - 识别后用当前激活账号自动提交确认，按设置可「成功后清空剪贴板 / 自动退出」。
5. **选项 / 日志**：标题栏图标打开弹窗（启动自动登录、窗口置顶、成功后动作、检测间隔、日志清空）。

## 配置与数据

配置及账号凭据保存在：

```
%APPDATA%\崩坏3扫码器\config.json
%APPDATA%\崩坏3扫码器\bh3-scanner.log
```

> 目录名沿用早期名称（`崩坏3扫码器`）以确保升级不丢失已登录账号。
> **工具全程不输入、不存储账号密码**，仅保存登录令牌（stoken / access_key，DPAPI 加密落盘）。

## 目录结构

```
src-tauri/            Rust 后端（Tauri v2）
  src/lib.rs          入口、命令接线、扫码登录会话管理
  src/scan.rs         全屏扫码主流程编排（自动识别渠道 + ROI 优先 + 退避去重）
  src/qr.rs           二维码解码 / SVG 生成
  src/screenshot.rs   全屏与区域截图（多显示器 + DPI）
  src/duplication.rs  DXGI 桌面复制（低 CPU 取帧）
  src/config.rs       配置结构与持久化
  src/crypto.rs       MD5 / HMAC / RSA / DPAPI
  src/http.rs         共享 HTTP 客户端
  src/sdk/bili.rs        B 站 TV 扫码登录 + 用户信息
  src/sdk/official.rs    米哈游通行证扫码登录 + combo/panda 扫码
  src/sdk/mihoyo.rs      米哈游 B 服 SDK（verify + 扫码确认 + OA 分发）
  src/sdk/hypergryph.rs   鹰角账号登录 + 扫码确认
  src/sdk/cred_login.rs   米哈游/B站 验证码 / 密码登录
  tests/              集成测试
src/                  前端
  index.html          主窗口界面
  verify.html         极验人机验证独立窗口
docs/                 维护者文档
  发展总纲.md          项目唯一事实来源（架构 / 缺陷清单 / 决策 / 路线图）
reference/            三个只读参考项目
```

## 注意事项

- 本工具仅用于登录本人游戏账号，请勿用于非授权用途。
- 服务端限流（如「操作过于频繁」-1001）为正常防护，稍等片刻重试即可。
- 二维码识别的准确率受截图清晰度影响；建议游戏登录界面二维码尽量清晰、无遮挡。

## 参考

- [bilibili-API-collect](https://github.com/SocialSisterYi/bilibili-API-collect)
- [mihoyo-api-collect](https://github.com/UIGF-org/mihoyo-api-collect)
- [MHY_Scanner](https://github.com/DSVVA/MHY_Scanner)
