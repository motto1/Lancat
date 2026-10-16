package fan.lancat.shortcut

import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.app.Person
import androidx.core.content.LocusIdCompat
import androidx.core.content.pm.ShortcutInfoCompat
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.core.graphics.drawable.IconCompat
import fan.lancat.MainActivity
import fan.lancat.avatar.Avatar
import fan.lancat.model.LancatRepository
import fan.lancat.model.Peer
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/**
 * Direct Share / 会话快捷方式：
 * - 最近 4 台设备（按最近消息时间，排除拉黑）发布成动态快捷方式，setDynamicShortcuts 整体替换、按重要度排序
 * - 每个快捷方式：category = share-target 的 SHARE_TARGET、setLongLived(true)、LocusId = 快捷方式 id、
 *   Person（与通知同一个 key）、图标 = 与 UI 同算法的自适应头像位图
 * - 收发消息时 pushDynamicShortcut 上报使用（提升分享面板排序），id 永不复用
 * - 同一个 id 也是 MessagingStyle 通知的 shortcutId，Android 11+ 通知因此进入「对话」分区
 */
object ShortcutPublisher {
    private const val TAG = "lancat-shortcut"
    const val PREFIX = "peer_"
    const val CATEGORY_SHARE_TARGET = "fan.lancat.category.SHARE_TARGET"
    const val MAX = 4

    fun shortcutId(peerId: String) = PREFIX + peerId
    fun peerIdOf(shortcutId: String): String? = shortcutId.takeIf { it.startsWith(PREFIX) }?.removePrefix(PREFIX)?.takeIf { it.isNotEmpty() }

    /** 选出要发布的设备：有过消息的按最近消息排序，其余（在线）补位，排除拉黑 */
    fun pick(peers: List<Peer>, max: Int = MAX): List<Peer> =
        peers.asSequence()
            .filter { !it.blocked }
            .sortedWith(compareByDescending<Peer> { it.lastMessage?.timeMs ?: Long.MIN_VALUE }.thenByDescending { it.online })
            .take(max)
            .toList()

    fun person(ctx: Context, peer: Peer): Person = Person.Builder()
        .setKey(peer.id)
        .setName(peer.name)
        .setIcon(icon(ctx, peer))
        .setImportant(true)
        .build()

    fun icon(ctx: Context, peer: Peer): IconCompat {
        val px = (108 * ctx.resources.displayMetrics.density).toInt().coerceIn(108, 432)
        return IconCompat.createWithAdaptiveBitmap(Avatar.bitmap(peer.id, peer.name, px, adaptive = true, gray = peer.blocked))
    }

    fun build(ctx: Context, peer: Peer, rank: Int = 0): ShortcutInfoCompat {
        val id = shortcutId(peer.id)
        val open = Intent(ctx, MainActivity::class.java)
            .setAction(Intent.ACTION_VIEW)
            .putExtra(MainActivity.EXTRA_PEER_ID, peer.id)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        return ShortcutInfoCompat.Builder(ctx, id)
            .setShortLabel(peer.name.ifBlank { "?" }.take(24))
            .setLongLabel(peer.name.ifBlank { "?" })
            .setIcon(icon(ctx, peer))
            .setIntent(open)
            .setLongLived(true)
            .setLocusId(LocusIdCompat(id))
            .setPerson(person(ctx, peer))
            .setCategories(setOf(CATEGORY_SHARE_TARGET))
            .addCapabilityBinding("actions.intent.SEND_MESSAGE")
            .setRank(rank)
            .build()
    }

    @OptIn(FlowPreview::class)
    fun start(ctx: Context, repo: LancatRepository, scope: CoroutineScope) {
        val app = ctx.applicationContext
        scope.launch {
            repo.peers
                .map { l -> pick(l).map { Triple(it.id, it.name, it.blocked) } to l }
                .distinctUntilChanged { a, b -> a.first == b.first }
                .debounce(1500)   // 动态快捷方式在后台有调用频率限制，合并抖动
                .collect { (_, all) -> publish(app, pick(all)) }
        }
    }

    fun publish(ctx: Context, top: List<Peer>) {
        try {
            val infos = top.mapIndexed { i, p -> build(ctx, p, i) }
            ShortcutManagerCompat.setDynamicShortcuts(ctx, infos)
        } catch (e: Exception) {
            Log.w(TAG, "setDynamicShortcuts: $e")
        }
    }

    /** 收到/发出消息时上报使用；也保证发会话通知前快捷方式已存在 */
    fun push(ctx: Context, peer: Peer) {
        try {
            ShortcutManagerCompat.pushDynamicShortcut(ctx, build(ctx, peer))
        } catch (e: Exception) {
            Log.w(TAG, "pushDynamicShortcut: $e")
        }
    }

    /** 设备被删除/清空记录时（暂无此事件；留给集成） */
    fun remove(ctx: Context, peerId: String) {
        try {
            ShortcutManagerCompat.removeLongLivedShortcuts(ctx, listOf(shortcutId(peerId)))
        } catch (_: Exception) {
        }
    }
}
