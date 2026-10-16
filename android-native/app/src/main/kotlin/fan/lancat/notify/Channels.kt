package fan.lancat.notify

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.os.Build
import fan.lancat.R

/**
 * 三个渠道分开：消息（HIGH，可弹出）、传输（LOW，静默；不能是 MIN，否则 Android 16 Live Updates 不会推广）、
 * 后台常驻（MIN，折叠在底部）。id 沿用旧版 msg / xfer / svc，升级后用户对旧渠道的设置保留；旧版的 done 渠道删除。
 */
object Channels {
    const val MSG = "msg"
    const val XFER = "xfer"
    const val SVC = "svc"
    private val OBSOLETE = listOf("done")

    fun ensure(ctx: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return
        nm.createNotificationChannels(
            listOf(
                NotificationChannel(MSG, ctx.getString(R.string.ch_msg), NotificationManager.IMPORTANCE_HIGH).apply {
                    description = ctx.getString(R.string.ch_msg_desc)
                    setShowBadge(true)
                },
                NotificationChannel(XFER, ctx.getString(R.string.ch_xfer), NotificationManager.IMPORTANCE_LOW).apply {
                    description = ctx.getString(R.string.ch_xfer_desc)
                    setShowBadge(false)
                },
                NotificationChannel(SVC, ctx.getString(R.string.ch_svc), NotificationManager.IMPORTANCE_MIN).apply {
                    description = ctx.getString(R.string.ch_svc_desc)
                    setShowBadge(false)
                },
            )
        )
        OBSOLETE.forEach { runCatching { nm.deleteNotificationChannel(it) } }
    }
}
