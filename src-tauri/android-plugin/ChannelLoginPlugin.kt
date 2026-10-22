package com.taumata.scanner.plugins

import android.app.Activity
import android.content.Intent
import android.content.res.Configuration
import android.os.Bundle
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
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
import com.nearme.game.sdk.GameCenterSDK
import com.nearme.game.sdk.callback.ApiCallback
import com.vivo.unionsdk.open.VivoAccountCallback
import com.vivo.unionsdk.open.VivoUnionSDK
import com.xiaomi.gamecenter.sdk.MiCommplatform
import com.xiaomi.gamecenter.sdk.MiErrorCode
import com.xiaomi.gamecenter.sdk.OnInitProcessListener
import com.xiaomi.gamecenter.sdk.entry.MiAppInfo
import cn.gundam.sdk.shell.even.SDKEventKey
import cn.gundam.sdk.shell.even.SDKEventReceiver
import cn.gundam.sdk.shell.even.Subscribe
import cn.gundam.sdk.shell.open.ParamInfo
import cn.gundam.sdk.shell.open.UCOrientation
import cn.gundam.sdk.shell.param.SDKParamKey
import cn.gundam.sdk.shell.param.SDKParams
import cn.uc.gamesdk.UCGameSdk
import org.json.JSONObject

@InvokeArg
class ChannelLoginArgs {
    var channel: String? = null
}

/**
 * 华为登录回调。
 *
 * 华为链路依赖 HMS SDK + 应用级 AGC 配置（自有华为开发者项目），因此其实现
 * （`HuaweiLogin`）只在凭据就位时才被同步进工程；主插件用**反射**调用它，
 * 这样"没有凭据"时本模块对 HMS 零编译期依赖。
 */
interface HuaweiLoginSink {
    /** 华为游戏服务已初始化，插件侧可发起授权 Intent。 */
    fun onReady()

    /** 成功：渠道凭证 JSON + 渠道 UID + 昵称。 */
    fun onDone(cred: String, uid: String, name: String)

    /** 失败原因。 */
    fun onError(message: String)
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
            // 网页登录型渠道（无厂商 SDK，QQ 互联登录页 + 自建凭证）。
            "yyb" -> startWebLogin(invoke, "yyb")
            "wegame" -> startWebLogin(invoke, "wegame")
            // 华为：条件启用（HMS SDK + 自有 AGC 配置），内部反射调用 HuaweiLogin。
            "huawei" -> loginHuawei(invoke)
            // OPPO（channel_id=18，GameCenterSDK；照参考项目 login/Oppo.java）。
            "oppo" -> loginOppo(invoke)
            // UC / 九游（channel_id=20，UCGameSdk；照参考项目 login/UC.java）。
            "uc" -> loginUC(invoke)
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
     * 当前等待 vivo 回调的 invoke（审计 P2-11）。
     *
     * `VivoUnionSDK.registerAccountCallback` 注册进 SDK 内部静态回调表，且 SDK 没有
     * unregister 接口。若每次登录都新建匿名 VivoAccountCallback，回调对象连同它捕获的
     * Invoke / Activity 会随登录次数无上限累积，旧对象也仍可能被 SDK 回调（对已结束的
     * invoke 重复应答）。这里改为：插件内只保留一个回调实例，每次登录只替换它消费的
     * invoke，消费后立即置空——重复回调被丢弃，不再累积可达状态。
     */
    @Volatile
    private var vivoInvoke: Invoke? = null

    private val vivoCallback = object : VivoAccountCallback {
        override fun onVivoAccountLogin(p0: String?, uid: String?, token: String?) {
            if (uid == null || token == null) {
                finishVivo(null, "vivo 登录返回空凭证")
                return
            }
            val cred = JSONObject()
            cred.put("authtoken", token)
            val obj = JSObject()
            obj.put("cred", cred.toString())
            obj.put("uid", uid)
            obj.put("name", uid)
            finishVivo(obj, null)
        }

        override fun onVivoAccountLogout(code: Int) {
            finishVivo(null, "vivo 账号登出（code=$code）")
        }

        override fun onVivoAccountLoginCancel() {
            finishVivo(null, "vivo 登录已取消")
        }
    }

    /** 单次消费：先取出并置空 invoke，再回 UI 线程应答；重复回调直接被丢弃。 */
    private fun finishVivo(obj: JSObject?, error: String?) {
        val inv = vivoInvoke ?: return
        vivoInvoke = null
        activity.runOnUiThread {
            if (error != null) inv.reject(error) else if (obj != null) inv.resolve(obj)
        }
    }

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
        // 上一次登录未收尾（用户中途返回等）：先让它失败，避免 JS 侧永久等待。
        vivoInvoke?.reject("vivo 登录被新的登录请求取代")
        vivoInvoke = invoke
        // 仍每次登录都注册（而不是只注册一次）：initSdk 是否会重置 SDK 内部回调表未知，
        // 而 vivo（B 服）本项目不做真机验证，保持原有时序风险最小；重复注册同一实例
        // 只是让表里多几个同引用项，不再新增可达状态。
        VivoUnionSDK.registerAccountCallback(activity, vivoCallback)
        VivoUnionSDK.login(activity)
    }

    // ---------- 网页登录渠道：应用宝 / Wegame（照参考项目 login/Tencent.java、Wegame.java） ----------

    /** 发起网页登录时暂存当前渠道 key（`@ActivityCallback` 只能拿到结果，拿不到入参）。 */
    private var pendingWeb: String? = null

    /**
     * 打开 QQ 互联登录页（ChannelWebLoginActivity），结果经 webLoginResult 回传。
     *
     * 应用宝：回调 URL 片段即 `access_token` + `openid` → 直接作为渠道凭证
     *   （`{"platform":1,"openid":..,"openkey":..,"is_teenager":false}`，channel_id=13）。
     * Wegame：先以该令牌调 `union_login` 换 Wegame 侧令牌（参考项目同款请求），
     *   再产出 `{"access_token":..,"openid":..,"uid":<pc_rail_id>}`（channel_id=27）。
     */
    private fun startWebLogin(invoke: Invoke, channel: String) {
        pendingWeb = channel
        val appId = if (channel == "wegame") {
            ChannelWebLoginActivity.APPID_WEGAME
        } else {
            ChannelWebLoginActivity.APPID_YYB
        }
        val intent = Intent(activity, ChannelWebLoginActivity::class.java)
            .putExtra(ChannelWebLoginActivity.EXTRA_APPID, appId)
        startActivityForResult(invoke, intent, "webLoginResult")
    }

    @ActivityCallback
    fun webLoginResult(invoke: Invoke, result: ActivityResult) {
        val channel = pendingWeb
        pendingWeb = null
        val data: Intent? = result.data
        if (result.resultCode != Activity.RESULT_OK || data == null) {
            invoke.reject(data?.getStringExtra(ChannelWebLoginActivity.EXTRA_RESULT_ERROR) ?: "已取消网页登录")
            return
        }
        val url = data.getStringExtra(ChannelWebLoginActivity.EXTRA_RESULT_URL) ?: ""
        val uin = data.getStringExtra(ChannelWebLoginActivity.EXTRA_RESULT_UIN) ?: ""
        val token = fragmentParam(url, "access_token")
        val openId = fragmentParam(url, "openid")
        if (token.isEmpty() || openId.isEmpty()) {
            invoke.reject("网页登录未返回 access_token / openid")
            return
        }
        if (channel == "wegame") {
            // union_login 需联网：改用工作线程，结果回主线程 resolve/reject。
            Thread {
                try {
                    val cred = wegameUnionLogin(openId, token)
                    val uid = cred.optString("uid", "")
                    if (uid.isEmpty()) {
                        activity.runOnUiThread { invoke.reject("Wegame 未返回账号标识") }
                    } else {
                        resolveCred(invoke, uid, uin.ifEmpty { uid }, cred.toString())
                    }
                } catch (e: Exception) {
                    val msg = e.message ?: "未知错误"
                    activity.runOnUiThread { invoke.reject("Wegame 换取登录态失败：$msg") }
                }
            }.start()
            return
        }
        val cred = JSONObject()
            .put("platform", 1)
            .put("openid", openId)
            .put("openkey", token)
            .put("is_teenager", false)
        resolveCred(invoke, openId, uin.ifEmpty { openId }, cred.toString())
    }

    /** 取 URL 片段（`#` 之后）中的参数值；不存在返回空串。 */
    private fun fragmentParam(url: String, key: String): String {
        val frag = url.substringAfter('#', "")
        for (kv in frag.split('&')) {
            val i = kv.indexOf('=')
            if (i > 0 && kv.substring(0, i) == key) return kv.substring(i + 1)
        }
        return ""
    }

    private fun resolveCred(invoke: Invoke, uid: String, name: String, cred: String) {
        val obj = JSObject()
        obj.put("cred", cred)
        obj.put("uid", uid)
        obj.put("name", name)
        activity.runOnUiThread { invoke.resolve(obj) }
    }

    /**
     * Wegame 侧登录态换取（照参考项目 Wegame.java 的 `union_login` 请求）。
     * 返回含 `access_token` / `openid` / `uid`(pc_rail_id) 的 JSON。
     */
    private fun wegameUnionLogin(openId: String, token: String): JSONObject {
        val body = JSONObject()
            .put("appid", "40000095")
            .put("app_key", "6dcc7197c6cec4cd050ead6262702302")
            .put("login_type", "game_login")
            .put("third_type", 0)
            .put("login_state_type", 2)
            .put("login_state_openid", openId)
            .put("login_state_token", token)
            .put("common_parameters", JSONObject().put("sdkversion", "3.1.1").put("plat", 0))
        val conn = java.net.URL("https://m.wegame.com.cn/web/oauth2.0/union_login")
            .openConnection() as java.net.HttpURLConnection
        conn.requestMethod = "POST"
        conn.connectTimeout = 15000
        conn.readTimeout = 15000
        conn.doOutput = true
        conn.setRequestProperty("Content-Type", "application/json")
        conn.outputStream.use { it.write(body.toString().toByteArray(Charsets.UTF_8)) }
        val text = conn.inputStream.bufferedReader().use { it.readText() }
        val resp = JSONObject(text)
        if (resp.optInt("code", -1) != 0) {
            throw IllegalStateException("union_login 返回 ${resp.optString("message", "code=" + resp.optInt("code"))}")
        }
        return JSONObject()
            .put("access_token", resp.optString("access_token"))
            .put("openid", resp.optString("openid"))
            .put("uid", resp.optString("pc_rail_id"))
    }

    // ---------- 华为（条件启用：HMS SDK + 自有 AGC 配置） ----------

    /**
     * 华为渠道登录（照参考项目 login/Huawei.java）。
     *
     * 反射调用 `HuaweiLogin`：该类由 patch-gen 步骤 5e 在
     * `vendor/huawei/agconnect-services.json` 存在时才同步进工程。未启用时这里
     * 给出明确错误提示，而不是崩溃或让主模块在编译期依赖 HMS SDK。
     */
    private fun loginHuawei(invoke: Invoke) {
        val sink = huaweiSink(invoke)
        try {
            huaweiCall(
                "init",
                arrayOf(Activity::class.java, HuaweiLoginSink::class.java),
                arrayOf(activity, sink)
            )
        } catch (e: Exception) {
            invoke.reject("华为渠道尚未启用（缺少 AGC 配置或 HMS 依赖）：${e.message}")
        }
    }

    @ActivityCallback
    fun huaweiSignIn(invoke: Invoke, result: ActivityResult) {
        val sink = huaweiSink(invoke)
        try {
            huaweiCall(
                "finish",
                arrayOf(Activity::class.java, Intent::class.java, HuaweiLoginSink::class.java),
                arrayOf(activity, result.data, sink)
            )
        } catch (e: Exception) {
            invoke.reject("华为登录处理失败：${e.message}")
        }
    }

    /** 华为链路的回调实现：`onReady` 拉起授权页，`onDone` 回传凭证。 */
    private fun huaweiSink(invoke: Invoke): HuaweiLoginSink = object : HuaweiLoginSink {
        override fun onReady() {
            activity.runOnUiThread {
                val intent = try {
                    huaweiCall(
                        "signInIntent",
                        arrayOf(Activity::class.java),
                        arrayOf(activity)
                    ) as Intent
                } catch (e: Exception) {
                    invoke.reject("华为授权页拉起失败：${e.message}")
                    return@runOnUiThread
                }
                startActivityForResult(invoke, intent, "huaweiSignIn")
            }
        }

        override fun onDone(cred: String, uid: String, name: String) {
            resolveCred(invoke, uid, name, cred)
        }

        override fun onError(message: String) {
            activity.runOnUiThread { invoke.reject(message) }
        }
    }

    /** 反射调用 HuaweiLogin 的静态方法（该类仅在华为凭据就位时才存在）。 */
    private fun huaweiCall(name: String, types: Array<Class<*>>, args: Array<Any?>): Any? {
        val cls = Class.forName("com.taumata.scanner.plugins.HuaweiLogin")
        return cls.getMethod(name, *types).invoke(null, *args)
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

    // ---------- UC / 九游（照参考项目 login/UC.java） ----------

    /**
     * 当前等待 UC 回调的 invoke（单次消费，照 vivoInvoke 的写法）。
     * UC 的回调注册在 SDK 内部且可能重复投递 → 消费后立即置空，重复回调被丢弃。
     */
    @Volatile
    private var ucInvoke: Invoke? = null

    /** UC SDK 是否已初始化（`ON_INIT_SUCC` 只回一次）。 */
    @Volatile
    private var ucInit = false

    private var ucSdk: UCGameSdk? = null

    /**
     * UC 事件接收器（`cn.gundam.sdk.shell.even.SDKEventReceiver`）。
     *
     * 与参考项目 login/UC.java 完全一致：`initSdk` 成功（ON_INIT_SUCC）后才
     * `login`；登录成功回调给出 `sid`，作为渠道凭证 `{"sid":..}` 交 Rust 侧以
     * channel_id=20 走 combo/granter 换取 combo 凭证。
     * 事件方法**不能是 private**：SDK 按注解分发，私有方法在部分实现上取不到
     * （参考项目是 Java 匿名类；Kotlin 侧显式放开可见性更稳）。
     */
    private val ucReceiver = object : SDKEventReceiver() {
        @Subscribe(event = intArrayOf(SDKEventKey.ON_INIT_SUCC))
        fun onInitSucc() {
            ucInit = true
            try {
                ucSdk?.login(activity, null)
            } catch (e: Exception) {
                finishUc(null, "UC 登录调用失败：${e.message}")
            }
        }

        @Subscribe(event = intArrayOf(SDKEventKey.ON_LOGIN_SUCC))
        fun onLoginSucc(sid: String) {
            if (sid.isEmpty()) {
                finishUc(null, "UC 登录返回空 sid")
                return
            }
            val cred = JSONObject().put("sid", sid)
            finishUc(cred, null, sid, sid)
        }

        @Subscribe(event = intArrayOf(SDKEventKey.ON_LOGIN_FAILED))
        fun onLoginFailed() {
            finishUc(null, "UC 登录失败（用户取消或渠道拒绝）")
        }

        @Subscribe(event = intArrayOf(SDKEventKey.ON_INIT_FAILED))
        fun onInitFailed() {
            finishUc(null, "UC SDK 初始化失败")
        }
    }

    private fun loginUC(invoke: Invoke) {
        ucInvoke?.reject("UC 登录被新的登录请求取代")
        ucInvoke = invoke
        try {
            if (!ucInit) {
                val sdk = UCGameSdk.defaultSdk()
                sdk.registerSDKEventReceiver(ucReceiver)
                val gpi = ParamInfo()
                // 参照实现：UC 只有 gameId（无 appId/appKey/密钥）。
                gpi.setGameId(UC_GAME_ID)
                gpi.setOrientation(
                    if (activity.resources.configuration.orientation == Configuration.ORIENTATION_PORTRAIT) {
                        UCOrientation.PORTRAIT
                    } else {
                        UCOrientation.LANDSCAPE
                    }
                )
                val params = SDKParams()
                params.put(SDKParamKey.GAME_PARAMS, gpi)
                sdk.initSdk(activity, params)
                ucSdk = sdk
            } else {
                ucSdk?.login(activity, null)
            }
        } catch (e: Exception) {
            finishUc(null, "UC SDK 初始化失败：${e.message}")
        }
    }

    /** 单次消费 UC 登录结果（与 finishVivo 同构）。 */
    private fun finishUc(cred: JSONObject?, error: String?, uid: String = "", name: String = "") {
        val inv = ucInvoke ?: return
        ucInvoke = null
        activity.runOnUiThread {
            if (error != null) {
                inv.reject(error)
            } else if (cred != null) {
                val obj = JSObject()
                obj.put("cred", cred.toString())
                obj.put("uid", uid)
                obj.put("name", name)
                inv.resolve(obj)
            }
        }
    }

    // ---------- OPPO（照参考项目 login/Oppo.java） ----------

    @Volatile
    private var oppoInvoke: Invoke? = null

    @Volatile
    private var oppoInit = false

    private var oppoSdk: GameCenterSDK? = null

    /**
     * OPPO 渠道登录。
     *
     * 链路照参考项目 login/Oppo.java：
     *   GameCenterSDK.init(OPPO_APP_KEY, activity) → getInstance()
     *   → doLogin(activity, cb) → onSuccess → doGetTokenAndSsoid(cb)
     *   → {token, ssoid} 作为渠道凭证（channel_id=18）交 Rust 侧换 combo 凭证。
     *
     * **与参考实现的默认路径一致，不做 PMS 伪装**：参考的 `OPPO_ADV_MODE` 默认为
     * false，此时 `Tools.verifyOfficialPack` 直接返回 true（不检测本体、不启用
     * `PmsHooker`）。本项目照此默认路径实现，不引入「把自家包伪装成
     * com.miHoYo.bh3.nearme.gamecenter」的钩子——那既是高风险反射改写，也与本项目
     * 「不伪装成其它应用」的取向冲突。
     */
    private fun loginOppo(invoke: Invoke) {
        oppoInvoke?.reject("OPPO 登录被新的登录请求取代")
        oppoInvoke = invoke
        if (!oppoInit) {
            GameCenterSDK.init(OPPO_APP_KEY, activity)
            oppoSdk = GameCenterSDK.getInstance()
            oppoInit = true
        }
        oppoSdk?.doLogin(activity, object : ApiCallback {
            override fun onSuccess(s: String?) {
                oppoSdk?.doGetTokenAndSsoid(object : ApiCallback {
                    override fun onSuccess(json: String?) {
                        try {
                            val o = JSONObject(json ?: "")
                            val token = o.getString("token")
                            val ssoid = o.getString("ssoid")
                            val cred = JSONObject().put("token", token).put("ssoid", ssoid)
                            finishOppo(cred, null, ssoid, ssoid)
                        } catch (e: Exception) {
                            // 参考实现在这里只打印、漏了失败回调；照抄流程但补上 reject，
                            // 否则 UI 会永远停在「登录中」。
                            finishOppo(null, "OPPO 登录返回内容无法解析：${e.message}")
                        }
                    }

                    override fun onFailure(msg: String?, code: Int) {
                        finishOppo(null, "OPPO 换取登录态失败：$msg（code=$code）")
                    }
                })
            }

            override fun onFailure(msg: String?, code: Int) {
                // 照参考实现：清掉 SDK 插件框架的缓存文件，避免上一次失败状态粘住。
                runCatching {
                    val f = java.io.File(activity.filesDir.parentFile, "shared_prefs/plugin_framework.xml")
                    if (f.exists()) f.delete()
                }
                finishOppo(null, "OPPO 登录失败：$msg（code=$code）")
            }
        })
    }

    /** 单次消费 OPPO 登录结果（与 finishVivo 同构）。 */
    private fun finishOppo(cred: JSONObject?, error: String?, uid: String = "", name: String = "") {
        val inv = oppoInvoke ?: return
        oppoInvoke = null
        activity.runOnUiThread {
            if (error != null) {
                inv.reject(error)
            } else if (cred != null) {
                val obj = JSObject()
                obj.put("cred", cred.toString())
                obj.put("uid", uid)
                obj.put("name", name)
                inv.resolve(obj)
            }
        }
    }

    companion object {
        /** vivo 渠道 appKey（参考项目 Constant.VIVO_APP_KEY）。 */
        private const val VIVO_APP_KEY = "94c93e8ac604d1909943862f12803ac9"

        /** B 站 SDK appKey（参考项目 Constant.BILI_APP_KEY）。 */
        private const val BILI_APP_KEY = "dbf8f1b4496f430b8a3c0f436a35b931"

        /** UC（九游）游戏 id（参考项目 login/UC.java 的 `gpi.setGameId(...)`）。 */
        private const val UC_GAME_ID = 654463

        /** OPPO 渠道 AppKey（参考项目 Constant.OPPO_APP_KEY）。 */
        private const val OPPO_APP_KEY = "f303388D89043bfEB1A667cfE42ea47E"
    }
}
