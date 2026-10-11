# 第三方许可证说明

本项目主体以 [MIT](LICENSE) 许可发布。以下按「需要特别关注 / 系统组件 / 其余依赖」分层说明。

## 需要特别关注的依赖

### zedbar（二维码解码）· LGPL-3.0-or-later

- 用途：二维码识别（`src-tauri/src/qr.rs`），本项目唯一解码器。
- 影响：LGPL 允许闭源使用，但**静态链接进二进制时**需履行「允许用户替换库」的义务
  （提供可重链接的目标文件或源码）。商用分发前请评估合规方案，或替换为宽松许可的
  解码器——如纯 Rust 的 rqrr（MIT，但实测识别率较低）。
- 选型记录：原生 C 版 libzbar（同为 LGPL，无增益）与 C++ 版 zxing-cpp（Apache-2.0，
  识别率更高，但二进制增量约 0.4–1.2 MB）均已评估，当前未采用。

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
