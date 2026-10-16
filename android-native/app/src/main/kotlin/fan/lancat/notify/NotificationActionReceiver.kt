package fan.lancat.notify

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.app.RemoteInput
import fan.lancat.ServiceLocator
import fan.lancat.service.ServiceController
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/** 通知按钮：回复、标为已读、暂停、继续、取消、重试。都不打开界面（打开/在文件夹中显示走 OpenFileActivity）。 */
class NotificationActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val action = intent.action ?: return
        val peerId = intent.getStringExtra(EXTRA_PEER)
        val messageId = intent.getStringExtra(EXTRA_MESSAGE)
        val replyText = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(MessageNotifier.KEY_REPLY)?.toString()
        val pending = goAsync()
        val app = context.applicationContext
        ServiceLocator.appScope.launch(Dispatchers.Default) {
            try {
                withTimeoutOrNull(9_000) { handle(app, action, peerId, messageId, replyText) }
            } catch (e: Exception) {
                Log.w("lancat", "notification action $action: $e")
            } finally {
                pending.finish()
            }
        }
    }

    private suspend fun handle(ctx: Context, action: String, peerId: String?, messageId: String?, replyText: String?) {
        val repo = ServiceLocator.repo
        when (action) {
            ACTION_REPLY -> if (peerId != null && !replyText.isNullOrBlank()) MessageNotifier.onReply(ctx, repo, peerId, replyText.trim())
            ACTION_MARK_READ -> if (peerId != null) MessageNotifier.onMarkRead(ctx, repo, peerId)
            ACTION_PAUSE -> messageId?.let { repo.pause(it) }
            ACTION_RESUME -> messageId?.let { ServiceController.ensureForTransfer(ctx); repo.resume(it) }
            ACTION_CANCEL -> messageId?.let { repo.cancel(it); Notify.cancel(ctx, TransferNotifier.tag(it), 2) }
            ACTION_RETRY -> messageId?.let {
                Notify.cancel(ctx, TransferNotifier.tag(it), 2)
                ServiceController.ensureForTransfer(ctx)
                repo.retry(it)
            }
        }
    }

    companion object {
        const val ACTION_REPLY = "fan.lancat.action.REPLY"
        const val ACTION_MARK_READ = "fan.lancat.action.MARK_READ"
        const val ACTION_PAUSE = "fan.lancat.action.PAUSE"
        const val ACTION_RESUME = "fan.lancat.action.RESUME"
        const val ACTION_CANCEL = "fan.lancat.action.CANCEL"
        const val ACTION_RETRY = "fan.lancat.action.RETRY"
        const val EXTRA_PEER = "fan.lancat.extra.PEER_ID"
        const val EXTRA_MESSAGE = "fan.lancat.extra.MESSAGE_ID"

        fun intent(ctx: Context, action: String, peerId: String? = null, messageId: String? = null): Intent =
            Intent(ctx, NotificationActionReceiver::class.java).setAction(action).apply {
                peerId?.let { putExtra(EXTRA_PEER, it) }
                messageId?.let { putExtra(EXTRA_MESSAGE, it) }
            }
    }
}
