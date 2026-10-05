//! 加密/签名工具：MD5、HMAC-SHA256、Python 风格 percent-quote、DPAPI。

use hmac::{Hmac, Mac};
use md5::{Digest as _, Md5};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// MD5，输出 32 位小写 hex。
pub fn md5_hex(data: &str) -> String {
    let mut h = Md5::new();
    h.update(data.as_bytes());
    hex::encode(h.finalize())
}

/// HMAC-SHA256，输出小写 hex。
pub fn hmac_sha256_hex(key: &str, data: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC 密钥不可为空");
    mac.update(data.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// 复刻 Python `urllib.parse.quote`（safe 默认 `/`）。
/// 用于 B 站 setSign 对 pwd 字段的转义。
pub fn py_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------- DPAPI（Windows 数据保护） ----------

/// DPAPI 密文前缀：带此前缀的字段视为已加密，否则按明文处理（兼容旧配置）。
pub const DPAPI_PREFIX: &str = "dpapi:";

/// 用 DPAPI 加密明文，返回 base64 密文（带前缀）。失败返回 None（调用方回退明文）。
/// 密钥由 Windows 绑定当前用户，无法在其它账户 / 机器上解密。
#[cfg(windows)]
pub fn dpapi_protect(plain: &[u8]) -> Option<String> {
    use base64::Engine as _;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        if CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .is_err()
        {
            return None;
        }
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let b64 = base64::engine::general_purpose::STANDARD.encode(slice);
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Some(format!("{DPAPI_PREFIX}{b64}"))
    }
}

#[cfg(not(windows))]
pub fn dpapi_protect(_plain: &[u8]) -> Option<String> {
    None
}

/// 解密 DPAPI 密文（不带前缀的 base64 部分）。失败返回 None。
#[cfg(windows)]
pub fn dpapi_unprotect(cipher_b64: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let raw = base64::engine::general_purpose::STANDARD
        .decode(cipher_b64)
        .ok()?;
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: raw.len() as u32,
            pbData: raw.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        if CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .is_err()
        {
            return None;
        }
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let v = slice.to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Some(v)
    }
}

#[cfg(not(windows))]
pub fn dpapi_unprotect(_cipher_b64: &str) -> Option<Vec<u8>> {
    None
}

/// 加密一个字符串字段（DPAPI），返回带前缀的密文。失败回退明文原样返回。
pub fn protect_secret(plain: &str) -> String {
    if plain.is_empty() {
        return String::new();
    }
    match dpapi_protect(plain.as_bytes()) {
        Some(c) => c,
        None => plain.to_string(),
    }
}

/// 解密一个字符串字段：带前缀走 DPAPI 解密（失败返回空，避免泄漏密文），
/// 不带前缀视为旧版明文原样返回。
pub fn unprotect_secret(stored: &str) -> String {
    match stored.strip_prefix(DPAPI_PREFIX) {
        Some(b64) => match dpapi_unprotect(b64) {
            Some(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            None => String::new(),
        },
        None => stored.to_string(),
    }
}
