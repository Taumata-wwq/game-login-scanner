package com.taumata.scanner.plugins

import android.app.Activity
import android.content.Intent
import android.text.TextUtils
import com.huawei.hms.api.HuaweiMobileServicesUtil
import com.huawei.hms.common.ApiException
import com.huawei.hms.jos.AppParams
import com.huawei.hms.jos.JosApps
import com.huawei.hms.jos.games.Games
import com.huawei.hms.jos.games.player.Player
import com.huawei.hms.support.account.AccountAuthManager
import com.huawei.hms.support.account.request.AccountAuthParams
import com.huawei.hms.support.account.request.AccountAuthParamsHelper
import com.huawei.hms.support.account.result.AccountAuthResult
import com.huawei.hms.utils.ResourceLoaderUtil
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import java.nio.charset.StandardCharsets

/**
 * 华为渠道登录（照参考项目 `login/Huawei.java`）。
 *
 * **本文件不参与常规构建**：它由 `scripts/android/patch-gen.ps1` 步骤 5e 在检测到
 * `vendor/huawei/agconnect-services.json` 时才同步进 `gen/android`。原因是华为联运
 * 需要应用级 AGC 配置（自有华为开发者项目），没有凭据时本模块不应进入工程；
 * 主插件 `ChannelLoginPlugin` 因此用**反射**调用本对象（见其 `loginHuawei`），
 * 保证无 HMS 依赖时主模块仍可编译、点击时给出明确提示而非崩溃。
 *
 * 链路：
 * 1. 华为游戏服务 `JosApps.init`（必须 init 成功后才能登录）
 * 2. `AccountAuthManager.getSignInIntent()` 授权（结果由插件侧 startActivityForResult 收回）
 * 3. `Games.getPlayersClient(activity).getGamePlayer(true)` 取 accessToken
 * 4. `POST jos-open-api.cloud.huawei.com/gameservice/api/gbClientApi` 换 `gameAuthSign`
 * 5. 组装 `{ts, cpid, playerId, playerLevel, playerSSign, time, is_teenager}` →
 *    Rust 侧以 channel_id=15 走 combo/granter 换米哈游凭证。
 *
 * 与参考项目的三处差异（见 docs/华为渠道AGC申请与接入指引-2026-10-17.md 第 8 节）：
 * - 防沉迷退出回调不 `System.exit(0)`，改为 `moveTaskToBack`（常驻扫码器不能被杀）；
 * - 不调用 `Games.getBuoyClient(activity).showFloatWindow()`（扫码器不是游戏本体）；
 * - 不做 `silentSignIn`，直接显式授权（扫码器无"上次登录成功"缓存语义）。
 */
object HuaweiLogin {

    /** 崩坏3 在华为渠道的 appId（请求体固定值，与本应用的 AGC app_id 无关）。 */
    private const val BH3_HUAWEI_APP_ID = "10624714"

    /** 崩坏3 在华为渠道的 cpid（凭证固定值）。 */
    private const val CPID = "890086000102026777"

    private const val GB_CLIENT_API =
        "https://jos-open-api.cloud.huawei.com/gameservice/api/gbClientApi"
    private const val HMS_APK_VERSION_CODE = "60700322"

    @Volatile
    private var initialized = false

    private fun authParams(): AccountAuthParams =
        AccountAuthParamsHelper(AccountAuthParams.DEFAULT_AUTH_REQUEST_PARAM_GAME).createParams()

    /** 初始化华为游戏服务；成功后回调 `sink.onReady()`，失败回调 `sink.onError()`。 */
    @JvmStatic
    fun init(activity: Activity, sink: HuaweiLoginSink) {
        if (initialized) {
            sink.onReady()
            return
        }
        HuaweiMobileServicesUtil.setApplication(activity.application)
        ResourceLoaderUtil.setmContext(activity)
        val appParams = AppParams(authParams()) {
            // 防沉迷/退出：参考项目直接 System.exit(0)，本应用仅退回后台。
            activity.moveTaskToBack(true)
        }
        JosApps.getJosAppsClient(activity).init(appParams)
            .addOnSuccessListener {
                initialized = true
                sink.onReady()
            }
            .addOnFailureListener { e ->
                val code = (e as? ApiException)?.statusCode
                sink.onError("华为游戏服务初始化失败（code=$code）")
            }
    }

    /** 取华为帐号授权 Intent（由插件侧发起 startActivityForResult）。 */
    @JvmStatic
    fun signInIntent(activity: Activity): Intent =
        AccountAuthManager.getService(activity, authParams()).getSignInIntent()

    /**
     * 处理授权结果：解析 → 取游戏角色 → 换 gameAuthSign → 产出渠道凭证。
     * 网络请求在工作线程执行，回调仍回主线程（由调用方保证）。
     */
    @JvmStatic
    fun finish(activity: Activity, data: Intent?, sink: HuaweiLoginSink) {
        val raw = data?.getStringExtra("HUAWEIID_SIGNIN_RESULT")
        if (TextUtils.isEmpty(raw)) {
            sink.onError("华为帐号授权未返回结果")
            return
        }
        val status = try {
            AccountAuthResult().fromJson(raw).status.statusCode
        } catch (e: Exception) {
            -1
        }
        if (status != 0) {
            sink.onError("华为帐号授权失败（code=$status）")
            return
        }
        Games.getPlayersClient(activity).getGamePlayer(true)
            .addOnSuccessListener { player ->
                Thread { fetchAuthSign(player, sink) }.start()
            }
            .addOnFailureListener { e ->
                val code = (e as? ApiException)?.statusCode
                sink.onError("获取华为游戏角色失败（code=$code）")
            }
    }

    /** 用 accessToken 换 gameAuthSign（参考项目同款请求）。 */
    private fun fetchAuthSign(player: Player, sink: HuaweiLoginSink) {
        try {
            val token = URLEncoder.encode(player.accessToken, StandardCharsets.UTF_8.name())
            val body = "extraBody=json%3D%7B%22appId%22%3A%22$BH3_HUAWEI_APP_ID%22%7D" +
                "&method=client.hms.gs.getGameAuthSign" +
                "&hmsApkVersionCode=$HMS_APK_VERSION_CODE" +
                "&accessToken=$token"
            val conn = URL(GB_CLIENT_API).openConnection() as HttpURLConnection
            conn.requestMethod = "POST"
            conn.connectTimeout = 15000
            conn.readTimeout = 15000
            conn.doOutput = true
            conn.setRequestProperty("Content-Type", "application/x-www-form-urlencoded")
            conn.outputStream.use { it.write(body.toByteArray(StandardCharsets.UTF_8)) }
            val text = conn.inputStream.bufferedReader().use { it.readText() }
            val resp = JSONObject(text)
            if (!resp.has("ts") || !resp.has("gameAuthSign")) {
                sink.onError("华为登录态换取失败（响应缺字段）")
                return
            }
            val cred = JSONObject()
                .put("ts", resp.get("ts"))
                .put("cpid", CPID)
                .put("playerId", resp.get("playerId"))
                .put("playerLevel", resp.getString("playerLevel"))
                .put("playerSSign", resp.getString("gameAuthSign"))
                .put("time", 0)
                .put("is_teenager", false)
            val uid = resp.optString("playerId", "")
            sink.onDone(cred.toString(), uid, player.displayName ?: uid)
        } catch (e: Exception) {
            sink.onError("华为登录态换取失败：${e.message}")
        }
    }
}
