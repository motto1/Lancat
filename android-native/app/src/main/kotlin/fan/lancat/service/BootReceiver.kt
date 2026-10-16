package fan.lancat.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import fan.lancat.SysPrefs

/**
 * 开机 / 应用更新后恢复常驻接收。组件默认禁用，只有常驻开关打开时 SysPrefs 才启用它。
 * 核实：Android 15 起 BOOT_COMPLETED 里不能启动 dataSync 等类型的前台服务，connectedDevice 不在禁止名单内；
 * 服务启动时 keepAlive=true → FgsPolicy 选 connectedDevice，所以可以从这里启动。
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED -> Unit
            else -> return
        }
        if (!SysPrefs.keepAlive(context)) return
        // ServiceController 的初始 keepAlive 来自 repo.settings；core 还没加载完时先用镜像值，保证首个 startForeground 用 connectedDevice
        ServiceController.primeKeepAlive(true)
        LancatService.ensure(context)
    }
}
