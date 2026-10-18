package fan.lancat.notify

import android.app.Activity
import android.app.PendingIntent
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.widget.Toast
import fan.lancat.R
import fan.lancat.service.LancatService
import fan.lancat.storage.ReceiveStorage

/**
 * 无界面跳板 Activity。通知的「打开」「在文件夹中显示」必须直接用 PendingIntent.getActivity
 * （Android 12 起禁止从通知触发的广播/服务里再开 Activity，即 notification trampoline 限制），
 * 而目标应用可能不存在，所以先进自己的透明 Activity，再尝试 + 兜底 + toast。
 * 也用作 Tile 的兜底：Android 14 上 TileService 里直接 startForegroundService 可能被拒，改为经此启动服务。
 */
class OpenFileActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val i = intent
        when (i.getStringExtra(EXTRA_MODE)) {
            MODE_OPEN -> {
                val uri = i.getStringExtra(EXTRA_URI)?.let(Uri::parse)
                val open = uri?.let { runCatching { ReceiveStorage.openIntent(this, it, i.getStringExtra(EXTRA_MIME)) }.getOrNull() }
                if (open != null) launch(open) else Toast.makeText(this, R.string.open_failed, Toast.LENGTH_SHORT).show()
            }
            MODE_REVEAL -> {
                val dir = i.getStringExtra(EXTRA_DIR) ?: ReceiveStorage.DEFAULT_DIR
                val candidates = ReceiveStorage.revealIntents(this, dir, i.getStringExtra(EXTRA_URI)?.let(Uri::parse))
                candidates.firstOrNull { tryStart(it) } ?: Toast.makeText(this, R.string.open_failed, Toast.LENGTH_SHORT).show()
            }
            MODE_START_SERVICE -> LancatService.ensure(this)
        }
        finish()
    }

    private fun launch(intent: Intent) {
        if (!tryStart(intent)) Toast.makeText(this, R.string.open_failed, Toast.LENGTH_SHORT).show()
    }

    private fun tryStart(intent: Intent): Boolean = try {
        startActivity(intent)
        true
    } catch (_: ActivityNotFoundException) {
        false
    } catch (_: Exception) {
        // SecurityException / FileUriExposed / 部分 ROM 的 IllegalArgument：只提示，不闪退
        false
    }

    companion object {
        private const val EXTRA_MODE = "mode"
        private const val EXTRA_URI = "uri"
        private const val EXTRA_MIME = "mime"
        private const val EXTRA_DIR = "dir"
        const val MODE_OPEN = "open"
        const val MODE_REVEAL = "reveal"
        const val MODE_START_SERVICE = "start_service"

        private fun base(ctx: Context, mode: String) = Intent(ctx, OpenFileActivity::class.java)
            .putExtra(EXTRA_MODE, mode)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_ANIMATION)

        fun pendingOpen(ctx: Context, messageId: String, uri: String, mime: String?): PendingIntent =
            PendingIntent.getActivity(
                ctx, ("open" + messageId).hashCode(),
                base(ctx, MODE_OPEN).setData(Uri.parse("lancat://open/" + Uri.encode(messageId))).putExtra(EXTRA_URI, uri).putExtra(EXTRA_MIME, mime),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )

        fun pendingReveal(ctx: Context, messageId: String, receiveDir: String, uri: String?): PendingIntent =
            PendingIntent.getActivity(
                ctx, ("reveal" + messageId).hashCode(),
                base(ctx, MODE_REVEAL).setData(Uri.parse("lancat://reveal/" + Uri.encode(messageId))).putExtra(EXTRA_DIR, receiveDir).putExtra(EXTRA_URI, uri),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )

        fun pendingStartService(ctx: Context): PendingIntent =
            PendingIntent.getActivity(ctx, 77, base(ctx, MODE_START_SERVICE), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    }
}
