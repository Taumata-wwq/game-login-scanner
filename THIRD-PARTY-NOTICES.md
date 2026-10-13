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

## 系统组件（随 Windows 提供，不随本项目分发）

- **Media Foundation H.264 解码器**（`CLSID_CMSH264DecoderMFT`）：直播视频解码。
  Windows 10/11 全系内置（N/K 版需安装「媒体功能包」）。
- **WebView2 运行时**：界面渲染。Win11 已内置；Win10 缺失时由安装包联网下载。

## 其余依赖

均为 MIT / Apache-2.0 / BSD / ISC 等宽松许可，包括但不限于：
tauri、wry、windows（Rust 绑定）、rustls、schannel、serde / serde_json、tokio、
rsa、sha2 / hmac / md-5、hex、rand、base64、uuid、log / env_logger、dirs、qrcodegen。

> 完整依赖清单可用 `cargo metadata --format-version 1` 生成；
> 逐包许可证文本以各依赖 crate 仓库为准。

## 其他说明

- 登录协议中的签名密钥 / 盐 / RSA 公钥均来自公开项目，非密码学安全，仅按社区惯例实现。
