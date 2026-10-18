package com.taumata.scanner.plugins

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.annotation.Keep

/**
 * 通知动作广播接收器：承接常驻通知上「停止扫码」按钮的点击。
 *
 * 为什么用广播而不是 PendingIntent.getActivity：
 * 用户在游戏里扫完想停时，若跳回应用再点按钮，还要受「回到应用后自动停止屏幕扫码」
 * 开关状态影响，结果不确定；广播直接在后台把"停止"意图交给应用，**不打断用户
 * 当前正在玩的游戏**（应用可在后台停掉扫描与屏幕共享）。
 *
 * 实现：收到后把意图转发给 MainActivity 的静态引用（若 Activity 存活），
 * 由 Activity 调 `window.__gsStopScanFromNotification()`；Activity 不在时忽略
 * （此时通常也并未在扫码）。
 */
@Keep
class NotifyActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context?, intent: Intent?) {
        if (intent?.action != NotifyBridge.ACTION_STOP_SCAN) return
        NotifyBridge.onStopScanAction()
    }
}
