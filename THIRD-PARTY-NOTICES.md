# 第三方许可证说明

本项目主体以 [MIT](LICENSE) 许可发布。以下按「需要特别关注 / 系统组件 / 其余依赖」分层说明。

## 解码与识别

### rxing（二维码解码）· Apache-2.0

- 用途：二维码识别（`src-tauri/src/qr.rs`），本项目唯一解码器（ZXing 的纯 Rust 移植）。
- 影响：宽松许可（MIT 兼容），静态链接无额外义务。
- 选型记录（2026-10-13 实测，24 帧真实 720p 直播画面 + 小码回归样本）：
  - **rxing**：4.5ms/帧，检出矩阵 6/6、70~200px 小码全过 → **采用**；
  - zedbar（LGPL-3.0，ZBar 算法移植）：11.3ms/帧，因性能与许可证（LGPL 静态链接义务）
    被替换；
  - zxing-cpp（C++，Apache-2.0，经 `zxing-cpp` Rust 封装）：26.7ms/帧（其默认选项含
    旋转/降采样多遍扫描，且封装未暴露调优开关），未采用。

  > zedbar 与 zxing-cpp 仍以 **dev-dependencies** 形式保留，仅用于解码器对照基准
  > （`tests/bench_qr*.rs`），**不进发布二进制**（零体积影响），是上述结论可复现的基础。

## 系统组件（随系统提供，不随本项目分发）

- **Media Foundation H.264 解码器**（`CLSID_CMSH264DecoderMFT`）：桌面端直播视频解码。
  Windows 10/11 全系内置（N/K 版需安装「媒体功能包」）。
- **WebView2 运行时**：桌面端界面渲染。Win11 已内置；Win10 缺失时由安装包联网下载。
- **Android 系统能力**：MediaProjection（屏幕捕获）、Camera2（摄像头取景）、
  MediaCodec（H.264 硬解）、Android Keystore（令牌加密）——均为操作系统组件。

## 其余依赖

均为 MIT / Apache-2.0 / Zlib 等宽松许可，包括但不限于：
tauri、wry、windows（Rust 绑定，桌面）、rustls、rustls-native-certs、schannel（桌面）、
serde / serde_json、tokio、rsa、sha2 / hmac / md-5、hex、rand、base64、
log / env_logger、qrcodegen、miniz_oxide（MIT OR Zlib OR Apache-2.0）。

Android 端专项依赖：

- anodecs（MIT）：MediaCodec 绑定；jni（MIT/Apache-2.0）：JNI 桥；
  ndk-context、android_logger（MIT OR Apache-2.0）。
- **webpki-roots（CDLA-Permissive-2.0）**：Android 端内置 Mozilla 根证书集（该系统无系统
  证书来源）。属允许型数据许可（Community Data License Agreement），商用分发无额外义务。

> 完整依赖清单可用 `cargo metadata --format-version 1` 生成；
> 逐包许可证文本以各依赖 crate 仓库为准。

## 渠道 SDK（不入库，由使用者自行提供）

第三方渠道登录依赖各渠道官方 SDK。这些 SDK **不在本仓库中**（`/vendor/` 已被
`.gitignore` 排除），需使用者自行从对应渠道开发者平台获取后放入 `vendor/`，再由
`scripts/android/patch-gen.ps1` 同步进构建。**未放置 `vendor/` 时构建照常成功，只是
渠道登录不可用**——补丁脚本对每个渠道都是「存在则启用、缺失则跳过」。

| 渠道 | 依赖 | 放入位置 | 缺失时的行为 |
|---|---|---|---|
| 小米 | `mio_sdk_base` 等 jar + `libconnectionbase.so` / `libmilinkconnection.so` | `vendor/mi_sdk/` | 跳过 `:mi_sdk` 模块 |
| vivo | `vivounionsdk` jar + `libvivo_account_sdk.so` | `vendor/channel_sdk/` | 跳过 jar / so 同步 |
| 应用宝、Wegame | `bsgamesdk_android_library` aar（内含 `lib39285EFA.so` / `libbsutils.so`） | `vendor/channel_sdk/` | 同上 |
| 华为 | `com.huawei.hms:game` / `hwid`（Maven）+ `agconnect-services.json` | `vendor/huawei/` | 跳过 AGC 配置与华为登录源码 |

注意事项：

- 上述 SDK 的**著作权与分发条款归各渠道方**，本项目仅按官方集成方式调用，不对其
  许可作任何陈述。自行构建并分发含这些 SDK 的安装包时，须自行确认符合对应渠道的
  开发者协议（多数渠道要求应用在其平台完成审核后才授权分发）。
- **B 站 SDK**（`bsgamesdk_android_library`）的 `classes.jar` 里捆绑了
  `com.paypal.android.sdk.*`（PayPal 移动 SDK，354 个类 / 约 1.8 MB 未压缩字节）
  与 `com.bsgamesdk.*` 全套 UI（667 个类 / 约 1.5 MB）。本项目只用其中的授权登录
  接口（`BSGameSdk.initialize` / `getInstance().login`），未使用任何支付功能；
  PayPal 类因 SDK 内部互引无法完全裁除，会留在 dex 中。
- 华为渠道另需 `agconnect-services.json`（含 `client/app_id` 等凭据），属使用者
  自有配置，同样不入库。

## 其他说明

- 登录协议中的签名密钥 / 盐 / RSA 公钥均来自公开项目，非密码学安全，仅按社区惯例实现。
