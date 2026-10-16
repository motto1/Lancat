package fan.lancat.perm

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import fan.lancat.SysPrefs

/**
 * 运行时权限（核实结论）：
 * - POST_NOTIFICATIONS：Android 13+ 运行时申请；拒绝后前台服务仍可运行，只是通知不显示（任务管理器里仍可见）
 * - NEARBY_WIFI_DEVICES：不需要。它管的是 Wi-Fi 扫描 / Wi-Fi Direct / Aware / 热点等 API；
 *   普通 TCP/UDP、组播、MulticastLock 不需要它（Android 16 上它只在开发者主动打开「局域网保护」兼容测试时才相关）
 * - ACCESS_LOCAL_NETWORK：Android 17 起 targetSdk≥37 的应用访问局域网（TCP 出入、UDP 单播/组播/广播、mDNS）必须获得，
 *   属于 NEARBY_DEVICES 组（用户已授予同组其他权限时不再弹窗）。本工程 targetSdk=37，所以在 Android 17+ 设备上必须申请；
 *   被拒时 TCP 连接超时、UDP/mDNS 报 EPERM，core 发现不了任何设备 —— UI 需要提示（Permissions.missing() 可查）
 * - 电池优化豁免：常驻开关打开时问一次（REQUEST_IGNORE_BATTERY_OPTIMIZATIONS），国产 ROM 上对常驻很关键
 */
class Permissions(private val activity: ComponentActivity) {
    private val launcher = activity.registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { res ->
        res.forEach { (perm, granted) ->
            if (!granted) SysPrefs.setInt(activity, "denied:$perm", SysPrefs.int(activity, "denied:$perm") + 1)
        }
    }

    fun requestStartupPermissions() {
        val need = missing(activity).filter { SysPrefs.int(activity, "denied:$it") < 2 }   // 拒绝两次后不再弹，由 UI 引导去设置
        if (need.isNotEmpty()) launcher.launch(need.toTypedArray())
        if (SysPrefs.keepAlive(activity)) maybeAskBatteryExemption()
    }

    @SuppressLint("BatteryLife")
    fun maybeAskBatteryExemption() {
        if (SysPrefs.flag(activity, "batteryAsked")) return
        val pm = activity.getSystemService(PowerManager::class.java) ?: return
        if (pm.isIgnoringBatteryOptimizations(activity.packageName)) return
        SysPrefs.setFlag(activity, "batteryAsked")
        runCatching {
            activity.startActivity(
                Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:${activity.packageName}"))
            )
        }
    }

    companion object {
        const val ACCESS_LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"   // = Manifest.permission.ACCESS_LOCAL_NETWORK（API 37）

        fun required(ctx: Context, sdk: Int = Build.VERSION.SDK_INT): List<String> = buildList {
            if (sdk >= 33) add(Manifest.permission.POST_NOTIFICATIONS)
            if (sdk >= 37 && ctx.applicationInfo.targetSdkVersion >= 37) add(ACCESS_LOCAL_NETWORK)
        }

        fun missing(ctx: Context): List<String> =
            required(ctx).filter { ContextCompat.checkSelfPermission(ctx, it) != PackageManager.PERMISSION_GRANTED }
    }
}
