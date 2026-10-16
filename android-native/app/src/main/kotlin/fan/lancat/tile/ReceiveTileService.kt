package fan.lancat.tile

import android.annotation.SuppressLint
import android.content.ComponentName
import android.content.Context
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import android.util.Log
import fan.lancat.R
import fan.lancat.ServiceLocator
import fan.lancat.SysPrefs
import fan.lancat.notify.OpenFileActivity
import fan.lancat.service.LancatService
import kotlinx.coroutines.launch

/**
 * 快速设置 Tile：开关「常驻接收」（Settings.keepAlive）。
 * 打开时立刻启动前台服务（connectedDevice）。核实：Android 14 上 TileService.onClick 里 startForegroundService
 * 可能抛 ForegroundServiceStartNotAllowedException（issuetracker 299506164 / 377528724；后续 AOSP 给 tile 点击加了
 * 15 秒临时白名单，但各版本/厂商不一），失败时退回 startActivityAndCollapse(PendingIntent) 经透明 Activity 启动。
 */
class ReceiveTileService : TileService() {

    override fun onStartListening() {
        super.onStartListening()
        render()
    }

    override fun onClick() {
        super.onClick()
        val on = !SysPrefs.keepAlive(this)
        SysPrefs.setKeepAlive(this, on)
        fan.lancat.service.ServiceController.primeKeepAlive(on)   // 让服务首次 startForeground 就用 connectedDevice
        render()
        ServiceLocator.appScope.launch {
            runCatching { ServiceLocator.repo.updateSettings { it.copy(keepAlive = on) } }
        }
        if (on && !LancatService.ensure(this)) collapseAndStart()
        // 关：ServiceController 收到 keepAlive=false，服务空闲后自行停止
    }

    @SuppressLint("StartActivityAndCollapseDeprecated")
    private fun collapseAndStart() {
        try {
            if (Build.VERSION.SDK_INT >= 34) {
                startActivityAndCollapse(OpenFileActivity.pendingStartService(this))
            } else {
                @Suppress("DEPRECATION")
                startActivityAndCollapse(
                    android.content.Intent(this, OpenFileActivity::class.java)
                        .putExtra("mode", OpenFileActivity.MODE_START_SERVICE)
                        .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
                )
            }
        } catch (e: Exception) {
            Log.w("lancat-tile", "collapseAndStart: $e")
        }
    }

    private fun render() {
        val t = qsTile ?: return
        val on = SysPrefs.keepAlive(this)
        t.state = if (on) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        t.label = getString(R.string.tile_label)
        if (Build.VERSION.SDK_INT >= 29) t.subtitle = getString(if (on) R.string.tile_on else R.string.tile_off)
        t.updateTile()
    }

    companion object {
        /** 设置在别处改了（应用内开关）时让系统重新拉一次 Tile 状态 */
        fun requestRefresh(ctx: Context) {
            try {
                requestListeningState(ctx, ComponentName(ctx, ReceiveTileService::class.java))
            } catch (_: Exception) {
            }
        }
    }
}
