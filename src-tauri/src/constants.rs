//! 常量模块：公开端点、密钥、盐与设备参数模板（均来自公开项目/公开 API）。

// ---------- B 站 ----------------

/// B 站 setSign 签名盐
pub const BILI_SIGN_SALT: &str = "dbf8f1b4496f430b8a3c0f436a35b931";

/// B 站 TV 扫码登录 appkey / appsec（公开的云视听小电视密钥对）
pub const BILI_TV_APPKEY: &str = "4409e2ce8ffd12b8";
pub const BILI_TV_APPSEC: &str = "59b43e04ad6965f34319062b478f83dd";

/// B 站 TV 扫码登录：申请二维码 / 轮询扫码状态
pub const BILI_TV_AUTH_CODE_URL: &str =
    "https://passport.bilibili.com/x/passport-tv-login/qrcode/auth_code";
pub const BILI_TV_POLL_URL: &str = "https://passport.bilibili.com/x/passport-tv-login/qrcode/poll";

// ---------- 米哈游 B 服 ----------------

/// 崩坏3 默认游戏业务键（`biz_key`），用于 `api-sdk.mihoyo.com/{biz_key}/...`。
/// B 服与官服的 BH3 `biz_key` 实测均为 `bh3_cn`；二维码若携带则以二维码为准。
pub const BH3_DEFAULT_BIZ_KEY: &str = "bh3_cn";

/// 米哈游 B 服 HMAC-SHA256 签名密钥
pub const MIHOYO_SIGN_KEY: &str = "0ebc517adb1b62c6b408df153331f9aa";

/// 崩坏3 B 服版本接口
pub const BH3_VERSION_URL: &str = "https://api-v2.scanner.hellocraft.xyz/v4/hi3_version";

/// B 服 OA 分发实时接口（MHY_Scanner 同款）。返回与当前游戏版本匹配的 dispatch，
/// 避免使用固化旧串导致的游戏端资源版本不匹配（每次登录下载数百 MB）。
pub const BH3_OA_REALTIME_URL: &str = "https://api.v6qbb.cloud/get_bh3_bilibili_oa";

/// 默认 OA token
pub const OA_TOKEN_DEFAULT: &str = "ebdda08dce6feb6bc552d393bae58c81";

/// 按游戏版本预置的 OA 分发串（来源 BBH3ScanLaunch version.json，社区实测可用）。
/// query_gameserver 的动态查询作为回退。
pub const OA_DISPATCH_PRESET: &[(&str, &str)] = &[
    ("9.0.0", "d2JnQo/BB3YcFmq1yBBSNC12Pnx1OFdDy1qkw5EwhMOrf2+9raDKiKhN3DTEhdRFFIgnsqt/Qed2eBUtaLgcorrt3NatKLzJpBYmfsSf0rflQ/rDBC7lxg3hDLboxs4nSfMvOftY6MwwXoV7TZo3AAcNiEQ7octY8+tfJQRs1Y8SnU3SW337/+ptypqtGsRSS1uCqqCYgwtuu9Ua7YNs398NZ1FH6RAIm/rumm4AvSa2CULEqw0soSW6vfuvRMmf3VwX/qwZb3k+KCZcbKTP1o6yUV2dkeCx19yeuekYjPGH3pXlskAtVwx3gJo7AGTbm0h9FnhOc/5uqWg1FqnvlRK6SxCP+OXta+eTOaHdVBiRhVUnNVe5pepwKYuBFhTZJk/aHIHkyE918JFtXuTg5ADvqInilf9uKG0cK8mtdRta5n69W4UwhW/hhgHESWa9i9J7YKV7dE8uCIe0JNWLsBuQsvqjU7+9+/y4NEz2nMIyPoJ5e6LeFzzuoHpLA4tAoBYpj+nupm921Ee4V7r9y2XbQun2w+jk8mB34w66MZhcLy/qNgZISTzsZ1IzsBL2/zIicV0fThAdKDay5lvcqytvIzgQjSxl1k4WtpixihF85I8EUtaQg5V5urITLgAjsZXbek9ch7hNN0HHECBuyMO1q5AVoMs9/DIk6YpjFA/6Iv4+6xoJoOtU5aNVpQf3XWYS8zRULEvE5TkqpC0HWHgNywMKo4gFRrIzkF9V9kYrXpt4CmrvwxQVWioQ4TQGj5/vUpXV9IL/qgCuhBPiZzaJDI4MAgLS+BaDyITxfLdkgAkmpcyNDwksQVg0/EDfuL9h2Sc3Np6eCyoCx287QGb9N227hZZTR2TG9htQAe7ArkaVNN2WW8NRbcrVErRD4KKjkM15dmsyeKpTayFZk5CUZI/WEW73AfaCxTznPAII60pR9F1QBmyChjN4ustSSvt6fMKB2l9MCyxmrDq79XlTh9AKhOyJlFuacUCfSS8+IQ7Fbj35HsOs5r7jfhpXDRDemVkoCxl/o5Sc3d3dW+kgVocpigf1/Xg25zl3uR0tK/qoqtGnLFKkQJXxz5qc3MN/GTYxCClT8y10ldPZlXNtYkRzOA9dHE6wdEflBz55ZzwZ27GANIEu8UoUyvv1qzGJkpDrxzt3nLGUI88TnbXoTsZI9vp3Q7zHl01aLbNUcPxuVWxEcopG9pYbLQ75h1zrcQFiPR+v8ZM/uwYMzfoihI731FOj6b2Y6B/M6iSk5dA9lyNTDvhQJnhIgx4VagYPYBfCNQfieE//T1lNRqbHkSWLgrHtEOfGMl2WyaE+Fs/rPrUWPZCRi6M1vXjvvpTGenL53LFiMTMKyo3N7+UpJLD6CXCQFI5tXZbopsCXojmRdUZ3WcFS9+NNrj8zSfVNhboHffqfXjpYfj1NXeJmbWSp5CF2+BsSlJeIGb7fkIlgeRITUxIZGtPbw2rHXJhTvN3WQ+j8jFM/S7XuLx2I8K6xlN+TEJfFikVBTxU9XKuDpcYprwVvi6E83Wgk2O5s3oVLPhr6wqn8REUuBfNBJ6lEay6orfnSqOTJy0nN+Kt5hUgpZXtWDebr8z9hq6E0s/L/ahtWXLbZPPoBiZXvD6hN8l6/+1oxeXgFZcsKEpoBXnz9nNlf0QQwubmr04VL6jsaDZzlWJjNk+2W4Ibfuj+gZY2kAfOAG7fQBL8KfTeKrU5DFcZXz4SwDQPF8qIkZv1yn6bz02/dmmyjy8rDoRvOVZtARXr/tO4uyqLWPoNK/sD22HFlTRCke6tshEvQfZDapqAVCZwWucVjypsdPTZBhVMlRILDpsE+KDa8qO26QmOK3B2uvWNweN4z9+hmt1vYNGitlf/+u3zRqbdLdrJW3zGIN3mtIw01QCEaupGgXBEjDpJi3mZbAyKQ0P1HU7/QNlNYG25cFZopQ3F/AWJB8oK4s6ENA7TLbo62XRn70h9rInz41tHdjOjZ3D/58cEaaSr/Uj05RN1ktFxfuhm75whMw4Vv3GR2fC85FFaTOOKJ5UBZNr9Nu0qDhP87BTmU1lPlLIRxJovGBXWfrMaRG4twmntwoLY+zpeBZh6nWqNvoXLjLq8006BByxIn7HhFv+6+EPqTFrfFHzRIln8M5FoZ0rJaN6Dw/0lf0z8j+9YKihxdNZgRDFH0nfLiKCrN+166la4LRTDe+QcV9hhOP0F3wgFmPGyWIen+F4bK216nFE+g7fjPnZnNKyS/dJdlDj+YIez7e0XAdy6T4EuR852L2tq2zs52osBE8ohufyjvn0DQ6/bFw+XrU4kRtkrJf+/oM8MydzILVtg9tytfbzb0oRY3mCcSLn736Ga3W9g0aK2V//67fNGpBE4Sm07rSbH1ZVAIZlXwruxTiy8x7D6d30CAp1b2G6poEQiCUNwP26l4RiwSsI2u7ipc0iVvcj8ARmGzkgNJ//ewFtVpYsMK6h5/Al8HTrzyOrlM4dPBbyG5ZblvQcJL9BOJlEEQRW9GgEjRAm3B9CNIR3Uq/C1qmCOcRjAlEZyLPap+T5x8FSJ19HQj9u53gZ1f0+NyVuXrugs7bTp1x4qYkU40jq0FZTVLcFl/+0+whVWVm6wOH/HZfM1WQkxqBBq2V3GkVd6O++NTRo+imgQ6pwWsRR0G87lhKdvU7tdIfoJ4gs/VRiPpX7/IMKgEpHRp8NXnTaqLyTLhOYyZXAOnGeEQTHDz6/XReCaMt5pu8NfO9XyGlWtHcX92S6aK82TCYS8gESFQt8miecHiv4Cj9YotzrezwgfFnZ0YYd+XbDGqRhLPVn/faypk3OW5qFs8jWOcMPoTIh7WLzCKX0KZK028C3V8ZC9IdzWO7GkOKhkAshUJLFP8YIHuSvJcHqTASMb2Huwpw5UZN4JxxtWMCBHDWQRIqWjyB/u6n1z+kk0aEYdpz0UpedLGjT979+p+b9KkEfHrMnBNYsEGChT0/pm73OapL+6CVmWQQRiDwaGEEBIMXT93RikKOanankj234Tur2mCP0wdRQFORWreH+0QnUzWNbGqCfaucZQve+7XT2CEomyLQoTO39snBLBL1fR8TT3nP3aiEp1wNTD5R8ReXtPy0czkwRC6WP0r/J81QqFQ35wXUuVe/PpTuEu+PSjGPT9IxDsKIHTFUUEmkczGvkHpI3HFkMiUOt/UiPNh9wryQLIVSCknXu95"),
    ("9.1.0", "RYBmVfe6Zp1v9mgtZgQCtSlswBte6W0MqkoAQCYtq1q9aJAPtgaUrKZnblWrow8DBpxHf6ND5bMleh5/6W6oeW8c47/qB+6O8UUpFTYOCMT8ahq7j2Tb5Acb6sRZkrU00lNhm8jO5X57G34z5nMqUQkK4AT4BM7p9sjuLjY06uq38DdynAEtaQYrkbxnN4A3ofqxNJcFX/2AfDp/BgL/8kWFYT/WUVV0vf6gDG8CcPX90NueMINvTdus7hoqAb6/81QC54nqGe91VhB2Nqo979qSnfEHIAX94m33EvSajpXPeG9r6Hr1evEGoafnClocQO5Zz40Yt3PpcMgZlSa735IWa180nKMqfMobMc/CXSpCxqGcHZxIXLUVBqnaMaw5hL+9/YKiUsR43ibXt6XFOaBVgwBcZKkr6N43cHbKp9F1ntjX20mGRiNZvUvR/H4yc267Oqpq30xNIoPjZYIpW3XtmPlNqbpTH3CYQWCuadtZuHFeg8rpyabYeGdl8Qg1Rx4DTZVYseDpHuI2fJDg0Sw4Cp50kCggBUO3jIbCvSTEvtNFD0IMyJvU3U0RjKLd2hc4Zb6Ofn/+GvQwSilOu5j+iWMRAJTm14cNHpsBD+sBFLZ8EKOmwwyQ0iRuiONFG12fs1HtUkjNr8LT0W9P6qL6xuxavnpDc2PcvYUjYDkk0Dr/M7nlWd6EXzdAEd/2hjLqMdjc72RapQyHvypsaJWz/o3Cufy9H4KqyE3sxdNtSfn/NDE9nh/kUMY1GdbmipCQw1LDv5aSHXvgfK61wqse7oS6ynuCpzQJkVN6Dry5cF+rYDvhuNiv3vFgEkgjb09gWZH01QtWJCeQij5LHYuE7ZrsQ4ZFvBmhwXQyKEZ484KU0rjfY673Cho+5axclK9m272ibejjbHUwyCtEoiAvOB2Zd218eucFa28wMHBjue2pUJY0vmhAKoiwAZVzVsJbRb3M6trmQbSXWH6FH892elaRWbBohdxp7y4DV/58/Tt8YuaGKTaEqOmb84aK3ViPZJNXUCE7KMrDiu49VABH6mghk+c6a8YBYWFioc0HcmjfW0EtzUM1lzn/shQPUhwJJ/n1SYvwowNLcj/pfESCEyAxOzN4lligMtRU/vIqf2jzJhvofEZoKb/yl20D0tksWkL+h+3pUWBNxmtOl+9B1sam5pt4yqNB6i8fXlwdGPj65DaG/7hVCqGiC5w/1nZr0gRBfKy5MQv6Wzk8lltmtbR71xiqrB1Wk1w4LkiVk1d15JoaW2vrTp7Q2KalmyEZgxQPRdiWSLvPXVuCkAZhxiuJbgdol4qJfOVxNeUaxRtxPKDQbRBcqMFivoQFp/SjTJuxJduAhjNoBO7PHwq/2RGkxJ+B3uFt21N6uYvPlH+wmEimh31KmsGuy6+N5VUxeWG2xez1PAUoOVtpwmCQ0JBzSuTpYf6O4bI1xzGiDT8JbTp1fNOQhBDCDUF508MvppKc7BKWvYyx7xkgac4BCgq2kyKNXdOvEx+a/uHGStzsMW5vNDD8JVgWh83qfAQ4iTsu6ubqYOtm37RsTi4SM4JY4YgVd1aydWlca4AECbSGhyHEOHiZi4yWhDOz8x3glHbJ8y+53vsCFHTU6JO2LTZU7J714fpb8AgcnRhHzcJsTHhe8moDSH3F2bVUR4pUYPalpR46hY1WcIlMwuzrGEbNYaTG0astkgWQPuYTerk5KkOxCpOxeIYd8jwZyMqJK23f8IW5N6ofU3mcI68G2lrJ+jXX+W90+1a+hivtAMlqbQfTwRKSWsVaXAJYul5cEghkPgUNxI4h4kPhCod1w0i98Tq6blHpqaBbFjQ+T3RrP8gXCJy5WFRFfE3AGhZPgkQT2dXsdQs4qG0IMajhaY78MSoUSqwyQ3l9W4g5SVGzCw/Ft3qdVFfT8W5wx+jJn8Bh13noXg4v/OKHhvvCBUDsUMKXLi0cS5+DGL1+boBifQ7Mi1wj9BrMoKQh3fmy3ADYYJ1AgTBWProwrUK5p/wY2+y1gx+AS7+rAOsEyDpETqHQ07MudTbqfkswb94mT0bzpNnZj0je9WkIEuRZ2t6OaPGvO9HVUDHQoqgI6eG6PkfYGYKObYr0tcdf7KofqriOf+8HHSRhESeRGgs3uFkLb+ptY78+x7s3nEParxeCPrOp2n0G4Ob+rxHL/+kHaGemtF9j3rxZubzkOVD+OE0A5yveap3hZQAO9XPNLTeCZNF1NLxC3bM1pu92Ly9kzd8lf+jvsn+F05RVI4gzuCP7qMVIQs8LL1tM2E6DfaW0pETknaZCByRpnoxbwdR+QAYLqDhMrzmbc/LYkhJpKh4Y819bvDiByrvuPfwaFk+CRBPZ1ex1CziobQgxCtODtfs2+bTU5EN5arjYzlDUMf2IyWMpwi7dtTVnowuApYWPzBacONZO3we4Ddm4mGL+pQwQ/96MkBbDiwGaxQ+mN3xAhVBBRDSxqgTNNr3i8T7XDBnec7v+vjI9BNA/9rWJPjxg+pJ/11ptX5XDsKQUDPp8/SFndmfatyzvtpCp3TlamPM7N9LLk8sGFfTfH5b+mco9Z5Cv1s/u/9eezRpy6MOBDZtUXKdwlxjiL0RNM3AzQLp7poXxdn2o/Vudc10pRR4RJWUKs8eT5pUZzd4vib1qihqBAognUstGT6KfqCyFtG9uTQVdsklAaCdDg4bQZXcnrNyzT7Foe/mz3F3nASPGsHS8+TGdv1mVSU5PME9ZMmPrWW6UhFDji94E4YznwDswYAbAlWVyjTUk8CcCQ/eWXSpKm7lN/qFpY5svCuk7KjxffvBBceaFVhSkHkXKjsHlE2VEFhJBVfUd7Ph4hGRRBR8O1wMke2pO0nO87+BxQLkstMUDkqvB9nP5kFT5qdjOaiLn7YjrQHd/WrQPgnT29qnGbLMsOOPBSghoS/H/35RO40ya9Jp/zQSWfZCxP/ciGCAVeiTLkeXad1vSHJuzWoghvDZCB+YNNmiJzc+pMyw+vPk3YbskUupUScJZ9eCvHahTpWKllJRWYzQ+RVfit17r3L025ZVEcxRdGXJE2lhP/bYt7ot33iI3giYREyXi5Q7uo6+BG1qkvslrRFx6A044jkPe/9t3dmyj8nfjZpCyGzm3MVDu8yhJx4rL8DGvQ8nLbGJ7v3cT5UP+vlgmFVH9ay3JcCgqizcDLdW9eqBFGLTV/cTNmUIm"),
];

/// 当前游戏版本对应的预置分发串。
pub fn preset_dispatch(version: &str) -> Option<&'static str> {
    OA_DISPATCH_PRESET
        .iter()
        .find(|(v, _)| *v == version)
        .map(|(_, d)| *d)
}
/// B 服 verify 端点
pub const MIHOYO_VERIFY_URL: &str =
    "https://api-sdk.mihoyo.com/bh3_cn/combo/granter/login/v2/login";

/// combo/panda 扫码基础地址（具体路径随二维码携带的 `biz_key` 变化）。
pub const PANDA_API_BASE: &str = "https://api-sdk.mihoyo.com";

/// 构造 combo/panda scan 端点：`{base}/{biz_key}/combo/panda/qrcode/scan`。
pub fn panda_scan_url(biz_key: &str) -> String {
    format!("{PANDA_API_BASE}/{biz_key}/combo/panda/qrcode/scan")
}

/// 构造 combo/panda confirm 端点。
pub fn panda_confirm_url(biz_key: &str) -> String {
    format!("{PANDA_API_BASE}/{biz_key}/combo/panda/qrcode/confirm")
}

// ---------- 官服 ----------------

/// 官服扫码登录：生成二维码 / 轮询扫码状态（米游社 App 扫码确认）
pub const OFFICIAL_QR_CREATE_URL: &str =
    "https://passport-api.mihoyo.com/account/ma-cn-passport/app/createQRLogin";
pub const OFFICIAL_QR_POLL_URL: &str =
    "https://passport-api.mihoyo.com/account/ma-cn-passport/app/queryQRLoginStatus";
/// 官服 v2 通行证 App 扫码确认端点（Cookie 携带 stoken+mid，替代 combo/panda）。
pub const OFFICIAL_SCAN_QR_URL: &str =
    "https://passport-api.mihoyo.com/account/ma-cn-passport/app/scanQRLogin";
pub const OFFICIAL_CONFIRM_QR_URL: &str =
    "https://passport-api.mihoyo.com/account/ma-cn-passport/app/confirmQRLogin";
// DSPROD header 固定字段（来源 DeviceFpManager.getDSPRODHeader）
// 用于 passport 扫码登录（createQRLogin / queryQRLoginStatus）。
pub const DSPROD_APP_ID: &str = "bll8iq97cem8";
pub const DSPROD_CLIENT_TYPE: &str = "2";
pub const DSPROD_GAME_BIZ: &str = "bbs_cn";
pub const DSPROD_APP_VERSION: &str = "2.107.0";
pub const DSPROD_SDK_VERSION: &str = "2.42.0";
pub const DSPROD_ACCOUNT_VERSION: &str = "2.42.0";
pub const DSPROD_DEVICE_FP_DEFAULT: &str = "1145141919810";

/// 官服 DS 签名盐（动态，仅 passport 登录类接口需要；`combo/panda` 扫码确认无需 DS）。
/// 值为空表示「暂不携带 DS 签名」（降级路径），待实测补充崩坏3通行证 SDK 的盐。
pub const OFFICIAL_DS_SALT: &str = "";

// ---------- 渠道账号登录（米哈游 / B站）----------

/// 米哈游通行证 Web 版 app_id（通行证登录页 user.mihoyo.com 使用，与游戏 App 不同）。
/// 极验 aigis 与 app_id 绑定，通行证短信/登录必须用此值。
pub const PASSPORT_WEB_APP_ID: &str = "dw9y09jqjpxc";
/// 米哈游通行证：申请短信验证码（需 x-rpc-aigis）。
pub const MIYOUSHE_SEND_SMS_URL: &str =
    "https://passport-api.mihoyo.com/account/ma-cn-verifier/verifier/createLoginCaptcha";
/// 米哈游通行证：验证码登录。用 App 版路径（返回 body 中的 stoken），
/// 但请求头沿用通行证 Web 协议（app_id 一致，极验 aigis 仍有效）。
pub const MIYOUSHE_LOGIN_SMS_URL: &str =
    "https://passport-api.mihoyo.com/account/ma-cn-passport/app/loginByMobileCaptcha";
/// 米哈游通行证：密码登录。
pub const MIYOUSHE_LOGIN_PWD_URL: &str =
    "https://passport-api.mihoyo.com/account/ma-cn-passport/app/loginByPassword";
/// 米哈游通行证 RSA 公钥（PKCS1_v1_5 加密手机号/账号/密码）。
/// 来源：社区公开实现 caisxg/mhylogin（enrsapw.py）。如失效需更新。
pub const MIYOUSHE_RSA_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDDvekdPMHN3AYhm/vktJT+YJr7\ncI5DcsNKqdsx5DZX0gDuWFuIjzdwButrIYPNmRJ1G8ybDIF7oDW2eEpm5sMbL9zs\n9ExXCdvqrn51qELbqj0XxtMTIpaCHFSI50PfPpTFV9Xt/hmyVwokoOXFlAEgCn+Q\nCgGs52bFoYMtyi+xEQIDAQAB\n-----END PUBLIC KEY-----";

/// B站：游戏 SDK 登录域名（rsa / start_captcha / login / user.info 均在此域名下）。
pub const BILI_SDK_LOGIN: &str = "https://line1-sdk-center-login-sh.biligame.net/";
/// B站游戏 SDK：RSA 公钥申请参数模板（app_id=180 崩坏3）。
pub const BILI_SDK_RSA_PARAM: &str = r#"{"operators":"5","merchant_id":"590","isRoot":"0","domain_switch_count":"0","sdk_type":"1","sdk_log_type":"1","timestamp":"1613035485639","support_abis":"x86,armeabi-v7a,armeabi","access_key":"","sdk_ver":"3.4.2","oaid":"","dp":"1280*720","original_domain":"","imei":"","version":"1","udid":"KREhESMUIhUjFnJKNko2TDQFYlZkB3cdeQ==","apk_sign":"4502a02a00395dec05a4134ad593224d","platform_type":"3","old_buvid":"XZA2FA4AC240F665E2F27F603ABF98C615C29","android_id":"84567e2dda72d1d4","fingerprint":"","mac":"08:00:27:53:DD:12","server_id":"378","domain":"line1-sdk-center-login-sh.biligame.net","app_id":"180","version_code":"510","net":"4","pf_ver":"12","cur_buvid":"XZA2FA4AC240F665E2F27F603ABF98C615C29","c":"1","brand":"Android","client_timestamp":"1613035486888","channel_id":"1","uid":"","game_id":"180","ver":"6.1.0","model":"MuMu"}"#;
/// B站游戏 SDK：登录参数模板。
pub const BILI_SDK_LOGIN_PARAM: &str = r#"{"operators":"5","merchant_id":"590","isRoot":"0","domain_switch_count":"0","sdk_type":"1","sdk_log_type":"1","timestamp":"1613035508188","support_abis":"x86,armeabi-v7a,armeabi","access_key":"","sdk_ver":"3.4.2","oaid":"","dp":"1280*720","original_domain":"","imei":"227656364311444","gt_user_id":"","seccode":"","version":"1","udid":"KREhESMUIhUjFnJKNko2TDQFYlZkB3cdeQ==","apk_sign":"4502a02a00395dec05a4134ad593224d","platform_type":"3","old_buvid":"XZA2FA4AC240F665E2F27F603ABF98C615C29","android_id":"84567e2dda72d1d4","fingerprint":"","validate":"","mac":"08:00:27:53:DD:12","server_id":"378","domain":"line1-sdk-center-login-sh.biligame.net","app_id":"180","pwd":"","version_code":"510","net":"4","pf_ver":"12","cur_buvid":"XZA2FA4AC240F665E2F27F603ABF98C615C29","c":"1","brand":"Android","client_timestamp":"1613035509437","channel_id":"1","uid":"","captcha_type":"1","game_id":"180","challenge":"","user_id":"","ver":"6.1.0","model":"MuMu"}"#;
/// B站游戏 SDK：验证码申请参数模板。
pub const BILI_SDK_CAPTCHA_PARAM: &str = r#"{"operators":"5","merchant_id":"590","isRoot":"0","domain_switch_count":"0","sdk_type":"1","sdk_log_type":"1","timestamp":"1613035486182","support_abis":"x86,armeabi-v7a,armeabi","access_key":"","sdk_ver":"3.4.2","oaid":"","dp":"1280*720","original_domain":"","imei":"227656364311444","version":"1","udid":"KREhESMUIhUjFnJKNko2TDQFYlZkB3cdeQ==","apk_sign":"4502a02a00395dec05a4134ad593224d","platform_type":"3","old_buvid":"XZA2FA4AC240F665E2F27F603ABF98C615C29","android_id":"84567e2dda72d1d4","fingerprint":"","mac":"08:00:27:53:DD:12","server_id":"378","domain":"line1-sdk-center-login-sh.biligame.net","app_id":"180","version_code":"510","net":"4","pf_ver":"12","cur_buvid":"XZA2FA4AC240F665E2F27F603ABF98C615C29","c":"1","brand":"Android","client_timestamp":"1613035487431","channel_id":"1","uid":"","game_id":"180","ver":"6.1.0","model":"MuMu"}"#;
// ---------- B 站设备参数模板（user.info 查询用，原样保留） ----------------

pub const BILI_USERINFO_PARAM: &str = r#"{"cur_buvid":"XZA2FA4AC240F665E2F27F603ABF98C615C29","client_timestamp":"1667057013442","sdk_type":"1","isRoot":"0","merchant_id":"590","dp":"1280*720","mac":"08:00:27:53:DD:12","uid":"437470182","support_abis":"x86,armeabi-v7a,armeabi","apk_sign":"4502a02a00395dec05a4134ad593224d","platform_type":"3","old_buvid":"XZA2FA4AC240F665E2F27F603ABF98C615C29","operators":"5","fingerprint":"","model":"MuMu","udid":"XXA31CBAB6CBA63E432E087B58411A213BFB7","net":"5","app_id":"180","brand":"Android","oaid":"","game_id":"180","timestamp":"1667057013275","ver":"6.1.0","c":"1","version_code":"510","server_id":"378","version":"1","domain_switch_count":"0","pf_ver":"12","access_key":"","domain":"line1-sdk-center-login-sh.biligame.net","original_domain":"","imei":"","sdk_log_type":"1","sdk_ver":"3.4.2","android_id":"84567e2dda72d1d4","channel_id":1}"#;
