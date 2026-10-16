package fan.lancat

import android.content.ComponentName
import android.content.Context
import android.content.pm.PackageManager
import fan.lancat.service.BootReceiver

/**
 * 系统层自己的小配置。keepAlive 是 Settings.keepAlive 的镜像：开机广播、Tile 要在 core 加载完之前就知道开关状态。
 * 真正的设置仍以 repo.settings 为准，镜像由 AppRuntime 在 settings 变化时写入。
 */
object SysPrefs {
    private const val FILE = "lancat_sys"

    private fun sp(ctx: Context) = ctx.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    fun keepAlive(ctx: Context): Boolean = sp(ctx).getBoolean("keepAlive", false)

    /** 写镜像，并同步开机自启组件：只有常驻开关打开时 BootReceiver 才处于启用状态 */
    fun setKeepAlive(ctx: Context, on: Boolean) {
        if (keepAlive(ctx) == on && bootEnabledMatches(ctx, on)) return
        sp(ctx).edit().putBoolean("keepAlive", on).apply()
        ctx.packageManager.setComponentEnabledSetting(
            ComponentName(ctx, BootReceiver::class.java),
            if (on) PackageManager.COMPONENT_ENABLED_STATE_ENABLED else PackageManager.COMPONENT_ENABLED_STATE_DISABLED,
            PackageManager.DONT_KILL_APP,
        )
    }

    private fun bootEnabledMatches(ctx: Context, on: Boolean): Boolean {
        val s = ctx.packageManager.getComponentEnabledSetting(ComponentName(ctx, BootReceiver::class.java))
        return (s == PackageManager.COMPONENT_ENABLED_STATE_ENABLED) == on
    }

    fun flag(ctx: Context, key: String): Boolean = sp(ctx).getBoolean(key, false)
    fun setFlag(ctx: Context, key: String, v: Boolean = true) = sp(ctx).edit().putBoolean(key, v).apply()
    fun int(ctx: Context, key: String): Int = sp(ctx).getInt(key, 0)
    fun setInt(ctx: Context, key: String, v: Int) = sp(ctx).edit().putInt(key, v).apply()
}
