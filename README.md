# 崩坏3 扫码器

一个体积小巧、启动快速、界面无冗余的《崩坏3》登录二维码扫码器，**仅支持 Windows 10+**。
界面为 Adobe / Clip Studio Paint 风格的专业黑白深色配色（中性灰阶 + 语义色仅用于状态指示），
无边框窗口 + 自绘标题栏，固定 300×500（可上下调节高度，最小 400）。

## 功能

- **B 服 + 官服双渠道**：支持 Bilibili（B 服）与米哈游通行证（官服），**完全自动识别**——
  崩坏3 PC 端两个渠道的游戏登录码形态相同（`qr_code_in_game.html?ticket=`），无法从 URL
  区分平台，工具在确认阶段按**当前已登录的账号**自动逐个尝试（优先上次成功的渠道）。
- **账号管理**：筛选栏（全部 / B 服 / 官服）+「添加账号」弹窗（**B 服与官服二维码同时
  生成**，用对应 App 扫哪个都行，无需切换渠道）；官服账号登录后显示米游社昵称。**B 服与官服可各激活一个账号并存**（同一渠道内单选），
  点击账号行即切换为当前使用账号。
- **扫码登录账号**：二维码方式用 **B 站 App**（B 服）或 **米游社 App**（官服）扫码确认；
  两个二维码同时展示，先确认者生效。
- **扫码进游戏**：点击「开始扫码」后先读取一次剪贴板图像；未命中则高频截屏搜索登录二维码。
  识别到游戏登录码后用当前激活账号提交确认。
- **自定义扫描区域**：主界面「扫描区域」面板内嵌**实时预览**（3:2 监视器，60fps，
  扫描状态与开始按钮集成在面板标题行），
  滚轮缩放、左键拖拽平移，**可视视口即扫描范围**；默认（未缩放）扫描全部显示器。
  每次启动恢复全屏，不持久化。截屏基于 DXGI 桌面复制：画面静止时近零 CPU。
- **成功频率限制**：2 分钟内扫码成功上限 10 次，防滥用 / 风控。
- **CPU 优化**：截屏基于 DXGI 桌面复制（静止近零成本）、ROI 优先解码、2×2 降采样快路径、
  全分辨率解码节流（400ms）、画面指纹去重、失败票据退避 + 日志去重。
- **无边框窗口**：自绘标题栏（可拖拽 + 最小化 / 关闭），固定 300×540；「选项与日志」为
  窗口内居中弹窗（含**启动时自动登录上次账号**、窗口置顶、成功后动作、检测间隔）。
- 主窗口关闭即退出（无残留进程），并**记住窗口位置**；全程不接触、不存储账号密码。
- **目标二维码校验**：只有能提取出登录票据的二维码才会触发登录确认；无关二维码会被忽略并
  继续扫描，对应渠道未登录账号时提示先登录。
- 正确处理 **高 DPI 与多显示器**。

## 技术栈

Rust + Tauri v2，前端为原生 HTML/CSS/JS（无 npm 依赖）。

| 模块 | 用途 |
| --- | --- |
| `rxing` | 二维码解码（降采样 / 放大重试 / 反色）与登录二维码 SVG 生成 |
| `arboard` | 剪贴板读取 |
| `screenshots` | 全屏 / 区域截图（多显示器 + DPI 换算） |
| `hmac` / `md-5` / `sha2` | 登录签名（HMAC-SHA256 / MD5） |
| `rsa` / `base64` | 账号密码 RSA 加密（PKCS1v15 + Base64） |
| `reqwest` | HTTP（进程级共享连接池，rustls） |
| `serde` + JSON | 数据持久化 |
| `windows` | 原生 Win32 API（保留给后续截图优化） |

## 登录协议

- **B 服**：B 站 TV 扫码登录（`passport-tv-login/qrcode/auth_code` + `poll`，appkey 签名）
  → 轮询取得 `access_token`（即游戏 SDK 的 access_key）与 mid。
- **官服**：米哈游通行证 App 扫码登录（`ma-cn-passport/app/createQRLogin` +
  `queryQRLoginStatus`，DSPROD 头）→ Confirmed 后取 tokens 中 token_type=1 的 stoken 与
  uid / mid。
- 两条渠道的**扫码确认共用** `api-sdk.mihoyo.com/bh3_cn/combo/panda/qrcode` 的 `scan` +
  `confirm`：
  - B 服：payload `proto=Combo`，账号上下文为 bh_info（open_id / combo_id / combo_token），
    走 HMAC-SHA256 签名。
  - 官服：payload `proto=Account`，账号上下文为 `uid` + `game_token`（由 stoken / mid 经
    `getGameToken` 换取），**无需签名 / DS**。

## 环境要求

- Windows 10 及以上（需已安装 **WebView2 运行时**，Win11 已内置）。
- 开发构建需安装 **Rust 工具链**（[rustup](https://rustup.rs/)）。

## 构建与运行（开发者）

```powershell
cd src-tauri
cargo run                 # 调试运行
cargo build --release     # 发布构建，产物在 target\release\bh3-scanner.exe
cargo test                # 单元 / 集成测试（tests\ 目录）
cargo test --test probe -- --ignored --nocapture   # 真实网络探针（需本机已保存账号）
```

> 注意：修改 `src/` 下的前端文件后，直接 `cargo build` 可能因增量编译不重新嵌入资源，
> 请先 `touch src/lib.rs`（或 `cargo clean -p bh3-scanner`）再构建。

## 使用说明

直接双击 `bh3-scanner.exe` 启动（窗口固定 300×540，不可缩放）。

1. **登录账号**：在「账号」区点击「添加账号」，选择渠道（B 服 / 官服）与登录方式：
   - **二维码**：点击「生成二维码」，用对应 App（B 服 = **B 站 App**，官服 = **米游社 App**）
     扫描并在手机上确认。
   - **账号密码**：输入账号密码登录；若触发验证码 / 风控，请改用二维码方式。
   同一渠道可登录多个账号，登录记录自动保存并设为当前激活账号。
2. **切换 / 删除账号**：点击账号行即切换为当前使用账号（单选圈高亮）；悬停账号行点「✕」删除。
3. **扫码进游戏**：将游戏停在登录界面（屏幕上出现登录二维码），点击「开始扫码」：
   - 先读取剪贴板中的二维码图片（限时 0.5 秒，被占用则跳过）；
   - 未命中则自动搜索登录二维码，**按已登录账号自动识别属于 B 服还是官服**；
   - 识别后用对应渠道当前激活的账号自动提交登录，按设置可「成功后清空剪贴板 / 自动退出」。
   - 再次点击红色「停止扫描」随时结束。
4. **选项 / 日志**：点击标题栏图标打开窗口内居中弹窗：
   - **启动**：启动时自动登录上次账号（自动开始扫描）、窗口置顶（盖在游戏上方）；
   - **扫码成功后**：自动退出、清空剪贴板；
   - **扫描参数**：全屏检测间隔；
   - **日志**：可一键清空。
   点 ×、按 Esc 或点弹窗外空白处关闭。顶栏可拖拽移动窗口，右侧为最小化与关闭按钮；关闭窗口即退出程序。

## 登录链路与已知问题

扫码确认（游戏码 → 用账号确认）已按参考项目逐字核对：

- **B 服 verify**：`combo/granter/login/v2/login` 的 `app_id`/`channel_id` 必须为数字、
  `data` 内 `uid` 必须为数字，否则服务器返回「渠道错误」(-107)。
- **OA 分发（PC）**：PC 客户端必须使用 `{版本}_gf_pc_bilibili` 标签的分发信息（安卓标签会让
  游戏端返回 errorCode 10030）；分发串是 base64 格式。
- **B 服确认载荷**：`ext.data` 对齐参考实现（`accountID` 留空 + `c` 携带 open_id），`ts` 用秒。
- **官服 game_token**：`getGameToken` 参数必须 URL 编码（stoken 含 `=` 等字符），且需携带最小
  `x-rpc-*` 请求头；`Accept` 与 `x-rpc-device_name` 头会导致该接口解析异常（已规避）。
- **服务端限流**：短时间高频请求会触发风控，错误表现为「操作过于频繁」(-1001) 或偶发的
  「decode err」——这是正常防护，稍等片刻重试即可，非工具缺陷。工具侧另有 2 分钟 10 次**成功**
  的自身限流。

## 支持的服务器范围

与参考项目一致，崩坏3 目前支持 **官服** 与 **B 服（Bilibili）** 两个服务器：

- 两者的 PC 端游戏登录码形态相同（`qr_code_in_game.html?ticket=`），确认时分别使用米哈游通行证
  令牌（官服）或 B 站 access_key（B 服）。
- **其他渠道服**（应用宝、小米、华为、OPPO、vivo、魅族、360、九游、WeGame 等）由各渠道 SDK
  独立登录，扫码确认需要对应渠道的账号令牌，属需逐个渠道实现 SDK 登录的较大工程，暂不在范围内；
  如需要可后续按渠道逐步补充（B 服 / 官服之外，WeGame、应用宝优先级相对高）。

## 配置与数据

配置及账号凭据保存在：

```
%APPDATA%\崩坏3扫码器\config.json
%APPDATA%\崩坏3扫码器\bh3-scanner.log
```

日志文件记录运行信息与 panic 现场（超过 2MB 自动轮转 .old），遇到闪退请附上它。

即 `C:\Users\<用户名>\AppData\Roaming\崩坏3扫码器\config.json`。**工具全程不输入、不存储
账号密码**，仅保存扫码 / 密码登录获得的令牌。

## 目录结构

```
src-tauri/            Rust 后端（Tauri v2）
  src/lib.rs          入口、命令接线、扫码登录会话管理
  src/scan.rs         全屏扫码主流程编排（自动识别渠道 + ROI 优先 + 退避去重）
  src/qr.rs           二维码解码 / SVG 生成
  src/screenshot.rs   全屏与区域截图（多显示器 + DPI）
  src/config.rs       配置结构与持久化
  src/http.rs         共享 HTTP 客户端
  src/sdk/bili.rs     B 站 TV 扫码登录 + 用户信息
  src/sdk/official.rs 米哈游通行证扫码登录 + combo/panda 扫码
  src/sdk/mihoyo.rs   米哈游 B 服 SDK（verify + 扫码确认 + OA 分发）
  src/sdk/password.rs 账号密码登录（官服 / B 服）
  tests/              集成测试
src/                  前端
  index.html          主窗口界面
docs/                 维护者文档
  发展总纲.md          项目唯一事实来源（架构 / 缺陷清单 / 决策 / 路线图）
reference/            三个只读参考项目
```

## 注意事项

- 本工具仅用于登录本人游戏账号，请勿用于非授权用途。
- B 服扫码登录获得的 TV access_token 与游戏 SDK 的兼容性以实测为准。
- 官服 `createQRLogin` / `loginByPassword` 若触发风控（`x-rpc-aigis`），工具会提示稍后重试。
- 二维码识别的准确率受截图清晰度影响；全屏找码时建议游戏登录界面二维码尽量清晰、无遮挡。
- B 服 OA 分发信息依赖版本查询接口，若该服务不可用，B 服扫码确认会提示网络错误。

## 参考

- [bilibili-API-collect：二维码登录（TV）](https://github.com/SocialSisterYi/bilibili-API-collect)
- [mihoyo-api-collect：米游社扫码登录](https://github.com/UIGF-org/mihoyo-api-collect)
- [MHY_Scanner](https://github.com/DSVVA/MHY_Scanner)
