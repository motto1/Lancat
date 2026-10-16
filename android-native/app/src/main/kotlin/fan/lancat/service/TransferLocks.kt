package fan.lancat.service

import android.annotation.SuppressLint
import android.content.Context
import android.net.wifi.WifiManager
import android.os.PowerManager
import android.util.Log

/**
 * 传输期间才持有的锁（空闲常驻时不拿，省电）：
 * - PARTIAL_WAKE_LOCK：灭屏后 CPU 不睡，传输不断。带 1 小时超时，传输仍在进行时每次 set(true) 续期。
 * - WifiLock WIFI_MODE_FULL_HIGH_PERF：灭屏时 Wi-Fi 不进省电模式。核实：API 34 起该模式被废弃，
 *   系统自动换成 WIFI_MODE_FULL_LOW_LATENCY，而低延迟锁只在「亮屏 + 应用在前台」时生效——
 *   所以 Android 14+ 灭屏后台传输时这把锁基本不起作用，只能靠前台服务 + 唤醒锁；仍然申请，对低版本有效。
 */
object TransferLocks {
    private const val TAG = "lancat-locks"
    private const val WAKE_TIMEOUT_MS = 60 * 60 * 1000L
    private var wake: PowerManager.WakeLock? = null
    private var wifi: WifiManager.WifiLock? = null

    @SuppressLint("WakelockTimeout")
    @Synchronized
    fun set(ctx: Context, on: Boolean) {
        val app = ctx.applicationContext
        try {
            if (on) {
                val w = wake ?: (app.getSystemService(Context.POWER_SERVICE) as PowerManager)
                    .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "lancat:transfer").apply { setReferenceCounted(false) }
                    .also { wake = it }
                w.acquire(WAKE_TIMEOUT_MS)
                val f = wifi ?: (app.getSystemService(Context.WIFI_SERVICE) as WifiManager)
                    .createWifiLock(@Suppress("DEPRECATION") WifiManager.WIFI_MODE_FULL_HIGH_PERF, "lancat:transfer")
                    .apply { setReferenceCounted(false) }
                    .also { wifi = it }
                if (!f.isHeld) f.acquire()
            } else {
                wake?.takeIf { it.isHeld }?.release()
                wifi?.takeIf { it.isHeld }?.release()
            }
        } catch (e: Exception) {
            Log.w(TAG, "set($on): $e")
        }
    }
}
