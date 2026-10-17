package com.taumata.scanner.plugins

import android.app.Activity
import android.os.Bundle
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.bsgamesdk.android.BSGameSdk
import com.bsgamesdk.android.callbacklistener.BSGameSdkError
import com.bsgamesdk.android.callbacklistener.CallbackListener
import com.bsgamesdk.android.callbacklistener.ExitCallbackListener
import com.bsgamesdk.android.callbacklistener.InitCallbackListener
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

    /** B 站 SDK 初始化状态（`BSGameSdk.initialize` 只需一次）。 */
    @Volatile
    private var biliInit = false

    @Command
    fun login(invoke: Invoke) {
        val args = invoke.parseArgs(ChannelLoginArgs::class.java)
        when (args.channel) {
            "xiaomi" -> loginXiaomi(invoke)
            "vivo" -> loginVivo(invoke)
            // B 站 SDK 授权登录（channel_id=14，与 B 服同一账号体系）。
            "bilibili" -> loginBilibili(invoke)
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

    // ---------- B 站 SDK 授权登录（照参考项目 login/Bilibili.java） ----------

    /**
     * B 站 SDK（BSGameSdk）授权登录。
     *
     * 与参考项目一致：`BSGameSdk.initialize(isDebug, activity, "590", "180", "378",
     * BILI_APP_KEY, initCb, exitCb)` 初始化 → `getInstance().login(cb)` 弹 B 站
     * 授权登录 UI → `onSuccess(Bundle)` 取 `uid` / `username` / `access_token`。
     *
     * 产出凭证沿用 B 服 combo 体系格式（`{"uid":..,"access_key":..}`，channel_id=14），
     * 因此 Rust 侧可直接复用既有 B 服校验与账号存储（`upsert_bili`）。
     *
     * 注意：初始化回调发生在**子线程**，`login` 与 `invoke.resolve` 均需回主线程。
     */
    private fun loginBilibili(invoke: Invoke) {
        if (biliInit) {
            biliLogin(invoke)
            return
        }
        BSGameSdk.initialize(
            true,
            activity,
            "590",
            "180",
            "378",
            BILI_APP_KEY,
            object : InitCallbackListener {
                override fun onSuccess() {
                    biliInit = true
                    activity.runOnUiThread { biliLogin(invoke) }
                }

                override fun onFailed() {
                    activity.runOnUiThread { invoke.reject("B 站 SDK 初始化失败") }
                }
            },
            // 退出回调：参考项目直接 System.exit(0)；本应用为常驻扫码器，
            // 不应被 SDK 的退出流程杀死，故仅忽略（退回后台）。
            ExitCallbackListener {
                activity.runOnUiThread { activity.moveTaskToBack(true) }
            }
        )
    }

    private fun biliLogin(invoke: Invoke) {
        BSGameSdk.getInstance().login(object : CallbackListener {
            override fun onSuccess(p0: Bundle?) {
                val uid = p0?.getString("uid") ?: ""
                val token = p0?.getString("access_token") ?: ""
                val name = p0?.getString("username") ?: ""
                if (uid.isEmpty() || token.isEmpty()) {
                    activity.runOnUiThread { invoke.reject("B 站登录返回空凭证") }
                    return
                }
                val cred = JSONObject()
                cred.put("uid", uid)
                cred.put("access_key", token)
                val obj = JSObject()
                obj.put("cred", cred.toString())
                obj.put("uid", uid)
                obj.put("name", name)
                activity.runOnUiThread { invoke.resolve(obj) }
            }

            override fun onFailed(err: BSGameSdkError?) {
                activity.runOnUiThread { invoke.reject("B 站登录失败（${err?.errorCode}）") }
            }

            override fun onError(err: BSGameSdkError?) {
                activity.runOnUiThread { invoke.reject("B 站登录异常（${err?.errorCode}）") }
            }
        })
    }

    companion object {
        /** vivo 渠道 appKey（参考项目 Constant.VIVO_APP_KEY）。 */
        private const val VIVO_APP_KEY = "94c93e8ac604d1909943862f12803ac9"

        /** B 站 SDK appKey（参考项目 Constant.BILI_APP_KEY）。 */
        private const val BILI_APP_KEY = "dbf8f1b4496f430b8a3c0f436a35b931"
    }
}
