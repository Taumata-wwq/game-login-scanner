package com.taumata.scanner.plugins

import android.app.Activity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.vivo.unionsdk.open.VivoAccountCallback
import com.vivo.unionsdk.open.VivoUnionSDK
import com.xiaomi.gamecenter.sdk.MiCommplatform
import com.xiaomi.gamecenter.sdk.MiErrorCode
import com.xiaomi.gamecenter.sdk.OnInitProcessListener
import com.xiaomi.gamecenter.sdk.entry.MiAppInfo
import org.json.JSONObject

@InvokeArg
class ChannelLoginArgs {
    var channel: String? = null
}

/**
 * 渠道登录插件（控制面）：Rust 经 PluginHandle::run_mobile_plugin 调用 `login`。
 *
 * 与参考项目 login/LoginInstanceManager 的「按 server_type 分发」对位——本插件为
 * 单一分发入口，内部按 channel 参数调用各厂商 SDK（2026-10-17 渠道扩展，首批小米）。
 *
 * 小米链路（照参考项目 login/Xiaomi.java）：
 *   MiCommplatform.Init(appId/appKey) → onUserAgreed → miLogin（SDK 弹登录 UI）
 *   → 成功回调返回 `{uid, session}` 凭证 JSON（由 Rust 侧 channel::verify 换取
 *   combo 凭证并保存账号）。
 *
 * 凭证为低频小数据，直接经命令返回（不经帧桥数据面）；session 等敏感值不落日志。
 */
@TauriPlugin
class ChannelLoginPlugin(private val activity: Activity) : Plugin(activity) {

    @Volatile
    private var miInit = false

    @Command
    fun login(invoke: Invoke) {
        val args = invoke.parseArgs(ChannelLoginArgs::class.java)
        when (args.channel) {
            "xiaomi" -> loginXiaomi(invoke)
            "vivo" -> loginVivo(invoke)
            else -> invoke.reject("暂不支持的渠道：${args.channel}")
        }
    }

    // ---------- 小米（照参考项目 login/Xiaomi.java） ----------

    private fun loginXiaomi(invoke: Invoke) {
        if (miInit) {
            xiaomiLogin(invoke)
            return
        }
        val appInfo = MiAppInfo()
        appInfo.setAppId("2882303761517502034")
        appInfo.setAppKey("5841750261034")
        MiCommplatform.Init(activity, appInfo, object : OnInitProcessListener {
            override fun finishInitProcess(loginMethod: List<String>?, gameConfig: Int) {
                miInit = true
                xiaomiLogin(invoke)
            }

            override fun onMiSplashEnd() {}
        })
    }

    private fun xiaomiLogin(invoke: Invoke) {
        MiCommplatform.getInstance().onUserAgreed(activity)
        MiCommplatform.getInstance().miLogin(activity) { code, arg1 ->
            val info = arg1
            if (code == MiErrorCode.MI_XIAOMI_PAYMENT_SUCCESS && info != null) {
                val cred = JSONObject()
                cred.put("uid", info.uid)
                cred.put("session", info.sessionId)
                val obj = JSObject()
                obj.put("cred", cred.toString())
                obj.put("uid", info.uid ?: "")
                obj.put("name", info.nikename ?: "")
                activity.runOnUiThread { invoke.resolve(obj) }
            } else {
                activity.runOnUiThread { invoke.reject("小米登录失败（code=$code）") }
            }
        }
    }

    // ---------- vivo（照参考项目 login/Vivo.java） ----------

    /**
     * vivo 渠道登录。
     *
     * 链路与参考项目一致：initSdk(appKey) → registerAccountCallback → login
     * （SDK 自弹登录 UI）→ onVivoAccountLogin 回调取 (uid, authtoken)，
     * 由 Rust 侧以 channel_id=19 + `{"authtoken":..}` 换取 combo 凭证。
     *
     * appKey 沿用参考项目 `Constant.VIVO_APP_KEY`（渠道服固定值）。
     */
    private fun loginVivo(invoke: Invoke) {
        // 参考项目传 BuildConfig.DEBUG；本应用统一以非调试模式初始化（登录链路一致，
        // 该标志只影响 SDK 自有的日志与调试提示）。
        VivoUnionSDK.initSdk(activity, VIVO_APP_KEY, false)
        VivoUnionSDK.registerAccountCallback(activity, object : VivoAccountCallback {
            override fun onVivoAccountLogin(p0: String?, uid: String?, token: String?) {
                if (uid == null || token == null) {
                    activity.runOnUiThread { invoke.reject("vivo 登录返回空凭证") }
                    return
                }
                val cred = JSONObject()
                cred.put("authtoken", token)
                val obj = JSObject()
                obj.put("cred", cred.toString())
                obj.put("uid", uid)
                obj.put("name", uid)
                activity.runOnUiThread { invoke.resolve(obj) }
            }

            override fun onVivoAccountLogout(code: Int) {
                activity.runOnUiThread { invoke.reject("vivo 账号登出（code=$code）") }
            }

            override fun onVivoAccountLoginCancel() {
                activity.runOnUiThread { invoke.reject("vivo 登录已取消") }
            }
        })
        VivoUnionSDK.login(activity)
    }

    companion object {
        /** vivo 渠道 appKey（参考项目 Constant.VIVO_APP_KEY）。 */
        private const val VIVO_APP_KEY = "94c93e8ac604d1909943862f12803ac9"
    }
}
