package com.taumata.scanner.plugins

import android.app.Activity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
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
}
