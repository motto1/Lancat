package fan.lancat.notify

import android.app.Notification
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import androidx.core.content.LocusIdCompat
import fan.lancat.AppForeground
import fan.lancat.MainActivity
import fan.lancat.R
import fan.lancat.model.Body
import fan.lancat.model.CoreEvent
import fan.lancat.model.Direction
import fan.lancat.model.LancatRepository
import fan.lancat.model.Peer
import fan.lancat.shortcut.ShortcutPublisher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import java.util.concurrent.ConcurrentHashMap

/**
 * 消息通知：每台设备一个 MessagingStyle 会话（tag = "msg:<peerId>"），Person 用与 UI 同算法的头像，
 * shortcutId / LocusId 与动态快捷方式一致（Android 11+ 进入「对话」分区，可设为优先对话）。
 * 操作：RemoteInput 直接回复（SEMANTIC_ACTION_REPLY）、标为已读（SEMANTIC_ACTION_MARK_AS_READ），都不打开界面。
 *
 * 气泡（BubbleMetadata）评估后不做：Lancat 以传文件为主、聊天是附带的；气泡需要一个可缩放的嵌入式会话 Activity，
 * 而 :ui 是单 Activity 导航，单独拆出会话页成本高；且 Android 16 起用户可以把任意对话通知手动转成气泡/浮窗（OEM 各异）。
 */
object MessageNotifier {
    const val KEY_REPLY = "fan.lancat.reply"
    private const val ID = 1
    private const val MAX_LINES = 8

    data class Line(val text: CharSequence, val timeMs: Long, val outgoing: Boolean)

    private val history = ConcurrentHashMap<String, ArrayDeque<Line>>()
    private val peersCache = ConcurrentHashMap<String, Peer>()

    fun tag(peerId: String) = "msg:$peerId"

    fun bodyText(body: Body): String = when (body) {
        is Body.Text -> body.text
        is Body.Sticker -> "[表情]"
        is Body.File -> "[文件] ${body.name}"
    }

    fun start(ctx: Context, repo: LancatRepository, scope: CoroutineScope) {
        val app = ctx.applicationContext
        scope.launch {
            repo.events.collect { e ->
                if (e !is CoreEvent.MessageReceived) return@collect
                val m = e.message
                if (m.dir != Direction.IN) return@collect
                val peer = repo.peers.value.firstOrNull { it.id == m.peerId } ?: return@collect
                if (peer.blocked) return@collect
                ShortcutPublisher.push(app, peer)                      // 上报使用 + 保证会话快捷方式存在
                if (!repo.settings.value.notifyMessages) return@collect
                if (AppForeground.mainVisible) return@collect          // 界面开着时由 UI 自己提示
                add(peer.id, Line(bodyText(m.body), m.timeMs, false))
                peersCache[peer.id] = peer
                post(app, peer, alert = true)
            }
        }
        // 未读清零（在 UI 里看过了、或别处标为已读）→ 收起通知
        scope.launch {
            repo.peers.collect { list ->
                for (p in list) {
                    if (p.unread == 0 && history.containsKey(p.id)) clear(app, p.id)
                    if (history.containsKey(p.id)) peersCache[p.id] = p
                }
            }
        }
    }

    private fun add(peerId: String, line: Line) {
        val q = history.getOrPut(peerId) { ArrayDeque() }
        synchronized(q) {
            q.addLast(line)
            while (q.size > MAX_LINES) q.removeFirst()
        }
    }

    private fun post(ctx: Context, peer: Peer, alert: Boolean) {
        val lines = history[peer.id]?.let { synchronized(it) { it.toList() } } ?: return
        Notify.post(ctx, tag(peer.id), ID, build(ctx, peer, lines, alert))
    }

    /** 直接回复：发出去后把自己的回复追加进会话并重发通知（否则系统一直转圈） */
    suspend fun onReply(ctx: Context, repo: LancatRepository, peerId: String, text: String) {
        repo.sendText(listOf(peerId), text)
        val peer = peersCache[peerId] ?: repo.peers.value.firstOrNull { it.id == peerId } ?: return
        ShortcutPublisher.push(ctx, peer)
        add(peerId, Line(text, System.currentTimeMillis(), true))
        post(ctx, peer, alert = false)
    }

    suspend fun onMarkRead(ctx: Context, repo: LancatRepository, peerId: String) {
        repo.markRead(peerId)
        clear(ctx, peerId)
    }

    fun clear(ctx: Context, peerId: String) {
        history.remove(peerId)
        Notify.cancel(ctx, tag(peerId), ID)
    }

    /** 纯构建（单测覆盖） */
    fun build(ctx: Context, peer: Peer, lines: List<Line>, alert: Boolean = true): Notification {
        val me = Person.Builder().setKey("self").setName(ctx.getString(R.string.me)).build()
        val them = ShortcutPublisher.person(ctx, peer)
        val style = NotificationCompat.MessagingStyle(me).setGroupConversation(false)
        for (l in lines) style.addMessage(NotificationCompat.MessagingStyle.Message(l.text, l.timeMs, if (l.outgoing) null else them))

        val rc = peer.id.hashCode()
        val open = PendingIntent.getActivity(
            ctx, rc,
            Intent(ctx, MainActivity::class.java)
                .setAction(Intent.ACTION_VIEW)
                .putExtra(MainActivity.EXTRA_PEER_ID, peer.id)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

        val remoteInput = RemoteInput.Builder(KEY_REPLY).setLabel(ctx.getString(R.string.act_reply_hint)).build()
        // RemoteInput 的 PendingIntent 必须 MUTABLE（系统要把输入写回 Intent）；显式组件，安全
        val replyPi = PendingIntent.getBroadcast(
            ctx, rc,
            NotificationActionReceiver.intent(ctx, NotificationActionReceiver.ACTION_REPLY, peerId = peer.id),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
        )
        val reply = NotificationCompat.Action.Builder(R.drawable.ic_stat, ctx.getString(R.string.act_reply), replyPi)
            .addRemoteInput(remoteInput)
            .setAllowGeneratedReplies(true)
            .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
            .setShowsUserInterface(false)
            .build()
        val readPi = PendingIntent.getBroadcast(
            ctx, rc,
            NotificationActionReceiver.intent(ctx, NotificationActionReceiver.ACTION_MARK_READ, peerId = peer.id),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val markRead = NotificationCompat.Action.Builder(R.drawable.ic_stat, ctx.getString(R.string.act_mark_read), readPi)
            .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ)
            .setShowsUserInterface(false)
            .build()

        val sid = ShortcutPublisher.shortcutId(peer.id)
        val incoming = lines.count { !it.outgoing }
        return NotificationCompat.Builder(ctx, Channels.MSG)
            .setSmallIcon(R.drawable.ic_stat)
            .setColor(0xFF0A84FF.toInt())
            .setStyle(style)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setShortcutId(sid)
            .setLocusId(LocusIdCompat(sid))
            .addPerson(them)
            .setContentIntent(open)
            .setAutoCancel(true)
            .setOnlyAlertOnce(!alert)
            .setNumber(incoming)
            .setWhen(lines.lastOrNull()?.timeMs ?: System.currentTimeMillis())
            .setShowWhen(true)
            .addAction(reply)
            .addAction(markRead)
            .build()
    }
}
