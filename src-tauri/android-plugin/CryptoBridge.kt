package com.taumata.scanner.plugins

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.annotation.Keep
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Android Keystore 密钥保护（敏感字段落盘加密）。
 *
 * 动机：[crypto.rs] 在 Windows 用 DPAPI 加密 access_key/stoken；Android 无 DPAPI，
 * 原实现回退**明文落盘**（发布前必须修复）。本对象用 **AndroidKeyStore** 的 AES-256-GCM
 * 密钥（硬件级，密钥不出 TEE/StrongBox；应用卸载即失效）加密敏感字段。
 *
 * 输出格式：base64(iv[12] || ciphertext+tag)，由 Rust 侧加 `keystore:` 前缀。
 */
@Keep
object CryptoBridge {

    init {
        System.loadLibrary("game_login_scanner_lib")
        nativeInit()
    }

    private external fun nativeInit()

    private const val KEYSTORE = "AndroidKeyStore"
    private const val KEY_ALIAS = "game_scanner_secrets_v1"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val GCM_TAG_BITS = 128
    private const val IV_LEN = 12

    /** 加密明文，返回 base64(iv||ciphertext)。 */
    @Keep
    @JvmStatic
    fun encrypt(plain: String): String {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, getOrCreateKey())
        val iv = cipher.iv
        val ct = cipher.doFinal(plain.toByteArray(Charsets.UTF_8))
        val out = ByteArray(iv.size + ct.size)
        System.arraycopy(iv, 0, out, 0, iv.size)
        System.arraycopy(ct, 0, out, iv.size, ct.size)
        return Base64.encodeToString(out, Base64.NO_WRAP)
    }

    /** 解密 base64(iv||ciphertext)，失败返回 null（Rust 侧置空令牌，避免误用）。 */
    @Keep
    @JvmStatic
    fun decrypt(b64: String): String? {
        return try {
            val raw = Base64.decode(b64, Base64.NO_WRAP)
            if (raw.size <= IV_LEN) return null
            val iv = raw.copyOfRange(0, IV_LEN)
            val ct = raw.copyOfRange(IV_LEN, raw.size)
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, getOrCreateKey(), GCMParameterSpec(GCM_TAG_BITS, iv))
            String(cipher.doFinal(ct), Charsets.UTF_8)
        } catch (_: Exception) {
            null
        }
    }

    private fun getOrCreateKey(): SecretKey {
        val ks = KeyStore.getInstance(KEYSTORE)
        ks.load(null)
        (ks.getEntry(KEY_ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        gen.init(
            KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build()
        )
        return gen.generateKey()
    }
}
