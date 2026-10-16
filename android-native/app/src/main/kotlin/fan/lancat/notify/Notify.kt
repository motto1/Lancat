package fan.lancat.notify

import android.Manifest
import android.annotation.SuppressLint
import android.app.Notification
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat

/** 发通知前统一检查 POST_NOTIFICATIONS（Android 13+）；没权限就静默丢弃 */
object Notify {
    fun canPost(ctx: Context): Boolean =
        (Build.VERSION.SDK_INT < 33 ||
            ContextCompat.checkSelfPermission(ctx, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) &&
            NotificationManagerCompat.from(ctx).areNotificationsEnabled()

    @SuppressLint("MissingPermission")
    fun post(ctx: Context, tag: String?, id: Int, n: Notification) {
        if (!canPost(ctx)) return
        try {
            NotificationManagerCompat.from(ctx).notify(tag, id, n)
        } catch (_: SecurityException) {
        }
    }

    fun cancel(ctx: Context, tag: String?, id: Int) = NotificationManagerCompat.from(ctx).cancel(tag, id)
}
