package fan.lancat.notify

import android.app.Notification
import android.app.PendingIntent
import android.content.Context
import android.os.Build
import android.os.SystemClock
import androidx.core.app.NotificationCompat
import fan.lancat.MainActivity
import fan.lancat.R
import fan.lancat.model.Body
import fan.lancat.model.CoreEvent
import fan.lancat.model.Direction
import fan.lancat.model.LancatRepository
import fan.lancat.model.TransferState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import java.util.concurrent.ConcurrentHashMap

/**
 * 传输通知（tag = "xfer:<messageId>"）。
 *
 * - 进行中：进度、速度、剩余时间；「暂停」「取消」。ongoing + onlyAlertOnce + 静默。
 * - Android 16+（API 36）：Notification.ProgressStyle（经 NotificationCompat.ProgressStyle），
 *   且对「用户发起的发送」申请 Live Updates（setRequestPromotedOngoing + 状态栏 chip 文字 setShortCriticalText）。
 *   接收是对方发起的，按 Live Updates 使用准则（不允许由他方触发）不申请推广。
 *   推广的硬性条件（官方文档核实）：POST_PROMOTED_NOTIFICATIONS 权限、ongoing、有标题、无自定义 RemoteViews、
 *   不是组摘要、不 colorized、渠道不是 IMPORTANCE_MIN；用户可在设置里关掉（canPostPromotedNotifications）。
 * - Android 15 及以下：NotificationCompat 普通进度条（setProgress，千分比）。
 * - 暂停：显示停住的进度，「继续」「取消」，可划掉。
 * - 完成：接收的 →「打开」「在文件夹中显示」；发送的 → 只提示已发送。
 * - 失败：原因 +「重试」（retryable 时）。取消：撤掉通知。
 * - 节流：同一状态下每个传输最多 1 秒更新一次（系统对单个应用的通知更新本来就有频率限制，过快会被丢弃）。
 */
object TransferNotifier {
    private const val ID = 2
    private const val THROTTLE_MS = 1000L

    data class Meta(
        val peerId: String,
        val peerName: String,
        val messageId: String,
        val name: String,
        val size: Long,
        val mime: String?,
        val localUri: String?,
        val dir: Direction,
    )

    private val metas = ConcurrentHashMap<String, Meta>()
    private val lastPost = ConcurrentHashMap<String, Pair<Long, Class<*>>>()

    fun tag(messageId: String) = "xfer:$messageId"

    fun start(ctx: Context, repo: LancatRepository, scope: CoroutineScope) {
        val app = ctx.applicationContext
        scope.launch {
            repo.events.collect { e ->
                if (e !is CoreEvent.TransferProgress) return@collect
                onProgress(app, repo, e.peerId, e.messageId, e.state)
            }
        }
    }

    fun onProgress(ctx: Context, repo: LancatRepository, peerId: String, messageId: String, state: TransferState) {
        val terminal = state is TransferState.Done || state is TransferState.Failed || state is TransferState.Cancelled
        val now = SystemClock.elapsedRealtime()
        val last = lastPost[messageId]
        if (!terminal && last != null && last.second == state.javaClass && now - last.first < THROTTLE_MS) return
        val meta = resolve(repo, peerId, messageId, refresh = state is TransferState.Done) ?: return
        if (state is TransferState.Cancelled) {
            Notify.cancel(ctx, tag(messageId), ID)
        } else {
            val n = build(ctx, meta, state, repo.settings.value.receiveDir)
            if (n != null) Notify.post(ctx, tag(messageId), ID, n)
        }
        if (terminal) {
            lastPost.remove(messageId)
            metas.remove(messageId)
        } else {
            lastPost[messageId] = now to state.javaClass
        }
    }

    private fun resolve(repo: LancatRepository, peerId: String, messageId: String, refresh: Boolean): Meta? {
        if (!refresh) metas[messageId]?.let { return it }
        val peerName = repo.peers.value.firstOrNull { it.id == peerId }?.name ?: peerId.take(8)
        val msg = repo.messages(peerId).value.lastOrNull { it.id == messageId }
        val f = msg?.body as? Body.File
        val meta = Meta(
            peerId = peerId,
            peerName = peerName,
            messageId = messageId,
            name = f?.name ?: "文件",
            size = f?.size ?: 0L,
            mime = f?.mime,
            localUri = f?.localUri,
            dir = msg?.dir ?: Direction.IN,
        )
        metas[messageId] = meta
        return meta
    }

    private fun action(ctx: Context, meta: Meta, action: String, label: Int): NotificationCompat.Action {
        val pi = PendingIntent.getBroadcast(
            ctx, (meta.messageId + action).hashCode(),
            NotificationActionReceiver.intent(ctx, action, peerId = meta.peerId, messageId = meta.messageId),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Action.Builder(0, ctx.getString(label), pi).build()
    }

    /** 纯构建（单测覆盖）。返回 null 表示该撤掉通知（已取消）。 */
    fun build(
        ctx: Context,
        meta: Meta,
        state: TransferState,
        receiveDir: String,
        sdk: Int = Build.VERSION.SDK_INT,
    ): Notification? {
        if (state is TransferState.Cancelled) return null
        val out = meta.dir == Direction.OUT
        val openChat = PendingIntent.getActivity(
            ctx, meta.peerId.hashCode(),
            android.content.Intent(ctx, MainActivity::class.java)
                .setAction(android.content.Intent.ACTION_VIEW)
                .putExtra(MainActivity.EXTRA_PEER_ID, meta.peerId)
                .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK or android.content.Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val b = NotificationCompat.Builder(ctx, Channels.XFER)
            .setSmallIcon(R.drawable.ic_notify)
            .setColor(0xFFD97757.toInt())
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .setContentIntent(openChat)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setShowWhen(false)

        val who = if (out) "发送给 ${meta.peerName}" else "来自 ${meta.peerName}"
        when (state) {
            is TransferState.Queued -> {
                b.setContentTitle(meta.name)
                    .setContentText("$who · 等待中")
                    .setOngoing(true)
                progress(b, sdk, 0, 0, indeterminate = true)
                if (out && sdk >= 36) b.setRequestPromotedOngoing(true).setShortCriticalText("…")
                b.addAction(action(ctx, meta, NotificationActionReceiver.ACTION_CANCEL, R.string.act_cancel))
            }
            is TransferState.Running -> {
                val total = if (state.total > 0) state.total else meta.size
                val pm = Format.permille(state.bytes, total)
                val parts = buildList {
                    add("${Format.bytes(state.bytes)} / ${if (total > 0) Format.bytes(total) else "?"}")
                    if (state.bytesPerSec > 0) add(Format.speed(state.bytesPerSec))
                    state.etaSec?.let { if (it >= 0) add(Format.eta(it)) }
                }
                b.setContentTitle(meta.name)
                    .setSubText(who)
                    .setContentText(parts.joinToString(" · "))
                    .setOngoing(true)
                progress(b, sdk, pm, 1000, indeterminate = total <= 0)
                if (out && sdk >= 36) b.setRequestPromotedOngoing(true).setShortCriticalText(Format.percent(state.bytes, total))
                b.addAction(action(ctx, meta, NotificationActionReceiver.ACTION_PAUSE, R.string.act_pause))
                b.addAction(action(ctx, meta, NotificationActionReceiver.ACTION_CANCEL, R.string.act_cancel))
            }
            is TransferState.Paused -> {
                val total = if (state.total > 0) state.total else meta.size
                b.setContentTitle(meta.name)
                    .setSubText(who)
                    .setContentText("已暂停 · ${Format.bytes(state.bytes)} / ${Format.bytes(total)}")
                    .setOngoing(false)
                progress(b, sdk, Format.permille(state.bytes, total), 1000, indeterminate = false)
                b.addAction(action(ctx, meta, NotificationActionReceiver.ACTION_RESUME, R.string.act_resume))
                b.addAction(action(ctx, meta, NotificationActionReceiver.ACTION_CANCEL, R.string.act_cancel))
            }
            is TransferState.Done -> {
                b.setContentTitle(if (out) "已发送给 ${meta.peerName}" else "已收到 ${meta.name}")
                    .setContentText(if (out) meta.name else "来自 ${meta.peerName}" + if (meta.size > 0) " · ${Format.bytes(meta.size)}" else "")
                    .setOngoing(false)
                    .setAutoCancel(true)
                    .setShowWhen(true)
                    .setCategory(NotificationCompat.CATEGORY_STATUS)
                if (!out) {
                    meta.localUri?.let { uri ->
                        val openPi = OpenFileActivity.pendingOpen(ctx, meta.messageId, uri, meta.mime)
                        b.setContentIntent(openPi)
                        b.addAction(NotificationCompat.Action.Builder(0, ctx.getString(R.string.act_open), openPi).build())
                    }
                    b.addAction(
                        NotificationCompat.Action.Builder(
                            0, ctx.getString(R.string.act_reveal),
                            OpenFileActivity.pendingReveal(ctx, meta.messageId, receiveDir, meta.localUri),
                        ).build()
                    )
                }
            }
            is TransferState.Failed -> {
                b.setContentTitle(if (out) "发送失败：${meta.name}" else "接收失败：${meta.name}")
                    .setContentText(state.reason.ifBlank { who })
                    .setStyle(NotificationCompat.BigTextStyle().bigText("$who\n${state.reason}"))
                    .setOngoing(false)
                    .setAutoCancel(true)
                    .setShowWhen(true)
                    .setCategory(NotificationCompat.CATEGORY_ERROR)
                if (state.retryable) b.addAction(action(ctx, meta, NotificationActionReceiver.ACTION_RETRY, R.string.act_retry))
            }
            TransferState.Cancelled -> return null
        }
        return b.build()
    }

    /**
     * API 36+：ProgressStyle（一整段 accent 色，按进度着色）；更低：普通进度条。
     * NotificationCompat.ProgressStyle 在 36 以下会退回默认样式而不画进度条，所以这里显式分支。
     */
    private fun progress(b: NotificationCompat.Builder, sdk: Int, value: Int, max: Int, indeterminate: Boolean) {
        if (sdk >= 36) {
            val style = NotificationCompat.ProgressStyle()
                .setStyledByProgress(true)
                .setProgressSegments(listOf(NotificationCompat.ProgressStyle.Segment(1000).setColor(0xFFD97757.toInt())))
                .setProgressIndeterminate(indeterminate)
            if (!indeterminate) style.setProgress(value * 1000 / max.coerceAtLeast(1))
            b.setStyle(style)
        } else {
            b.setProgress(max, value, indeterminate)
        }
    }
}
