package fan.lancat.service

import android.content.pm.ServiceInfo

/**
 * 前台服务类型选择（核实结论，详见 STATUS-c.md 版本差异表）：
 *
 * - 常驻接收（keepAlive）→ connectedDevice：「与外部设备经网络连接交互」，前置条件是声明
 *   CHANGE_WIFI_MULTICAST_STATE（已声明）；没有时长上限；不在 Android 15 的 BOOT_COMPLETED 禁止名单里，可开机启动。
 * - 只有传输、未开常驻 → dataSync：Android 15+（targetSdk≥35）每 24 小时累计 6 小时，到点回调
 *   onTimeout(startId, fgsType)，几秒内必须 stopSelf；用户把应用切到前台会重置额度；
 *   Android 15 起不能从 BOOT_COMPLETED 启动（我们开机只启动 connectedDevice）。
 * - 两者不同时用：常驻时 connectedDevice 已覆盖传输，避免 dataSync 的 6 小时把常驻服务一起拖死。
 * - API < 29：类型只是声明，startForeground 不带类型。
 */
object FgsPolicy {
    fun types(keepAlive: Boolean, sdk: Int): Int = when {
        sdk < 29 -> 0
        keepAlive -> ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
        else -> ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
    }

    /** 服务是否该继续跑 */
    fun shouldRun(s: ServiceController.State, nowMs: Long): Boolean =
        s.keepAlive || s.active > 0 || s.sending > 0 || nowMs < s.graceUntilMs
}
