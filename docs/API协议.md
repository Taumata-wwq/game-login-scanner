# 各渠道登录协议与踩坑记录

> 本文档记录米哈游 / B站 / 鹰角三方的登录接口协议、参数与**实测踩坑**，供维护者排查。
> 协议可能随服务端更新失效；如遇异常请对照本文档核对。
> 最后更新：2026-10-06。

---

## 一、米哈游（官服 / 米游社通行证）

### 1.1 扫码登录（App 协议）

| 步骤 | 接口 | 说明 |
| --- | --- | --- |
| 生成二维码 | `POST passport-api.mihoyo.com/account/ma-cn-passport/app/createQRLogin` | 返回 url + ticket |
| 轮询状态 | `POST .../app/queryQRLoginStatus` | Confirmed 时取 tokens 中 token_type=1 → stoken |
| 扫码确认 | `combo/panda/qrcode/scan` + `confirm` | 见 1.3 |

**DSPROD 请求头**（缺一不可，否则「请求频繁」-1001）：
```
x-rpc-app_id: bll8iq97cem8
x-rpc-client_type: 2
x-rpc-device_id: <uuid>
x-rpc-device_fp: <真实指纹，见 1.4>
x-rpc-device_name / device_model / sys_version
x-rpc-game_biz: bbs_cn
x-rpc-app_version: 2.107.0
x-rpc-sdk_version / account_version: 2.42.0
```

### 1.2 验证码 / 密码登录（通行证 Web 协议）★ 核心踩坑

**发短信**与**登录**用**不同协议**，这是最大陷阱：

| 步骤 | 接口 | 协议 | 关键头 |
| --- | --- | --- | --- |
| 申请短信 | `POST passport-api.mihoyo.com/account/ma-cn-verifier/verifier/createLoginCaptcha` | **通行证 Web** | `app_id=dw9y09jqjpxc`/`client_type=4`/`game_biz=plat_cn` |
| 验证码登录 | `POST .../ma-cn-passport/app/loginByMobileCaptcha` | **App** | `app_id=bll8iq97cem8`/`client_type=2`/`bbs_cn` |

**踩坑 1：极验 aigis 与 app_id 强绑定**
- 极验返回的 `gt` 取决于请求的 app 上下文。通行证 Web 环境返回 `gt=555874...`，游戏 App 环境返回 `gt=caf244...`。
- 若用 Web 环境完成的极验结果去调 App 接口，服务端校验失败 → **`-302` 图片验证码失效**。

**踩坑 2：`x-rpc-mi_referrer` 决定 gt 环境**
- 缺失此头 → 服务端返回「游戏 App 环境」的极验配置（gt=caf244...）。
- 必须携带通行证登录页完整 URL：
  `https://user.mihoyo.com/login-platform/index.html?app_id=dw9y09jqjpxc&theme=passport&token_type=4&game_biz=plat_cn&...&iframe_level=1#/login/captcha`

**踩坑 3：stoken 只在 App 版 body 中**
- Web 版 `loginByMobileCaptcha` 成功只返回 **Set-Cookie**（ltoken_v2/cookie_token_v2/ltuid），**无 stoken**。
- App 版返回 **body 的 `data.token.token`**（token_type=1 = stoken）+ `user_info.{aid,mid}`。
- 游戏扫码登录必须用 stoken，故登录步骤走 **App 版**。

**踩坑 4：极验 GT4 初始化参数**（浏览器抓包实证）
```js
initGeetest4({
  captchaId,            // = gt（use_v4=true 时）
  product: 'bind',
  language: 'zho',
  hideSuccess: true,
  userInfo: JSON.stringify({ session_id }),  // 注意是 session_id 不是 mmt_key
  riskType: 'icon'      // 或 'slide'，来自米哈游 data.risk_type
}, cb)
```
- `getValidate()` 返回 5 字段：`captcha_id`/`lot_number`/`pass_token`/`gen_time`/`captcha_output`。
- **过滤额外字段**：`userInfo` 会并入 getValidate() 结果污染 aigis，需只保留上述 5 个。
- aigis 格式：`session_id;base64(JSON(5字段))`，放 `x-rpc-aigis` 头。

### 1.3 米哈游游戏码扫码确认（combo/panda）

两条渠道共用 `api-sdk.mihoyo.com/{biz_key}/combo/panda/qrcode` 的 `scan` + `confirm`：
- **B 服**：payload `proto=Combo`，账号上下文为 bh_info（open_id/combo_id/combo_token），走 HMAC-SHA256 签名；`app_id`/`channel_id` 必须为数字，否则「渠道错误」-107。
- **官服**：payload `proto=Account`，用 `uid` + `game_token`（stoken/mid 经 getGameToken 换取），无需签名。

### 1.4 设备指纹 getFp

- 接口：`POST public-data-api.mihoyo.com/device-fp/api/getFp`
- 参数：`device_id`、`seed_id`(UUID)、`seed_time`(ms)、`platform=2`、`app_name=bh3_cn`、`ext_fields`
- 返回真实 32 位 hex 指纹。**假指纹必触发 -1001**。
- 当前 `ext_fields` 传空 `{}`；风控升级时可能需补全设备信息。

---

## 二、B站（B 服）

### 2.1 TV 扫码登录（已实现）

| 步骤 | 接口 |
| --- | --- |
| 申请二维码 | `passport.bilibili.com/x/passport-tv-login/qrcode/auth_code` |
| 轮询 | `.../qrcode/poll` |

- appkey/appsec：`4409e2ce8ffd12b8` / `59b43e04ad6965f34319062b478f83dd`（公开云视听小电视密钥）
- 签名：参数按 key 排序拼 `k=v&`（去尾）后 + appsec 做 MD5
- 轮询 Confirmed 时取 `access_token`（= 游戏 access_key）与 `mid`

### 2.2 验证码 / 密码登录（已实现，未真机验证）

| 步骤 | 接口 |
| --- | --- |
| 申请极验 | `GET passport.bilibili.com/x/passport-login/captcha` → `data.geetest.{gt,challenge}` + `data.token` |
| 发短信 | `POST .../x/passport-login/sms/send`（tel RSA 加密 + 极验字段） |
| 验证码登录 | `POST .../x/passport-login/login/sms` |
| 密码登录 | `POST .../x/passport-login/web/login` |

- RSA 公钥：`GET .../web/key` → `data.key` + `data.hash`（盐）
- 加密：`RSA(salt + 明文)` → base64
- 极验为**第三代（GT3）**：返回 `geetest_challenge`/`geetest_validate`/`geetest_seccode`，本项目用逗号拼接回传
- 凭证在 **Set-Cookie**（DedeUserID/bili_jct）

> ⚠️ B站登录参数基于通用 API 知识实现，**未真机验证**，可能需按实测调整。

---

## 三、鹰角（终末地 / 明日方舟）

- 固定 UA：`Skland/1.5.1 (com.hypergryph.skland; build:100501001; Android 33; ) Okhttp/4.11.0`
- 短信登录 + `scan_login` / `update_scan_status` 扫码确认（协议来自 `yu402317/skland-scan`）
- 二维码：`hypergryph://scan_login?scanId=`

---

## 四、极验（geetest）集成

- **GT3（B站）**：SDK `gt.0.4.9.js`，`initGeetest({gt, challenge})`，getValidate 返回 3 字段。
- **GT4（米哈游）**：SDK `v4/gt4.js`，`initGeetest4({captchaId})`，getValidate 返回 5 字段。
- 本项目统一在**独立验证窗口** `src/verify.html` 内完成，自动识别代际。
- 前端判定：有 `session_id` → GT4；有 `gt`+`challenge` 无 `session_id` → GT3。

---

## 五、通用踩坑

1. **CSP**：极验需 `unsafe-eval` + 多域名，当前 `csp: null`（禁用）。
2. **SDK 加载时机**：`gt4.js` 需页面加载即引入（采集行为数据），动态加载会导致初始化失败。
3. **服务端限流**：`-1001`/`-3101` 为正常防护，稍等重试即可。
4. **配置目录**：新目录 `游戏扫码器`，旧 `崩坏3扫码器` 首次启动自动迁移（保留旧目录）。
