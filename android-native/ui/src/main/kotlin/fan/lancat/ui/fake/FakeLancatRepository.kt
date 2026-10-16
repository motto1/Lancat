package fan.lancat.ui.fake

import fan.lancat.model.Body
import fan.lancat.model.CoreEvent
import fan.lancat.model.Direction
import fan.lancat.model.LancatRepository
import fan.lancat.model.Message
import fan.lancat.model.MessagePreview
import fan.lancat.model.Peer
import fan.lancat.model.SelfInfo
import fan.lancat.model.Settings
import fan.lancat.model.TransferState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.util.concurrent.atomic.AtomicLong

/**
 * 预览、截图测试和 :app 调试模式用的假仓库。全部在内存里，不碰网络，绝不会给真实设备发东西。
 *
 * @param now     「现在」，截图测试传固定值保证可复现
 * @param live    true 时模拟传输进度推进、对方回消息和「正在输入」；截图测试传 false
 * @param empty   true 时没有任何设备（用来截空态）
 * @param imageUri 图片消息用的本地 uri（截图测试放一张真图，展示缩略图）；null 时用不存在的 uri，显示为文件卡片
 */
class FakeLancatRepository(
    private val now: Long = System.currentTimeMillis(),
    private val live: Boolean = true,
    empty: Boolean = false,
    private val imageUri: String? = null,
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default),
) : LancatRepository {

    private val seq = AtomicLong(1000)
    private fun nextId(p: String) = "$p-${seq.incrementAndGet()}"

    private val _self = MutableStateFlow<SelfInfo?>(SelfInfo("a3f9c2d17be04e51", "Motto 的 Pixel", "192.168.1.23", 53317))
    override val self: StateFlow<SelfInfo?> = _self.asStateFlow()

    private val _events = MutableSharedFlow<CoreEvent>(extraBufferCapacity = 64)
    override val events: SharedFlow<CoreEvent> = _events

    private val _settings = MutableStateFlow(
        Settings(
            deviceName = "Motto 的 Pixel",
            receiveDir = "",
            theme = "system",
            keepAlive = true,
            autoOpenReceived = false,
            notifyMessages = true,
        )
    )
    override val settings: StateFlow<Settings> = _settings.asStateFlow()

    private val basePeers: MutableStateFlow<List<Peer>> = MutableStateFlow(if (empty) emptyList() else seedPeers())
    private val store = HashMap<String, MutableStateFlow<List<Message>>>()
    private val allFlow = MutableStateFlow<List<Message>>(emptyList())
    private val _peers = MutableStateFlow<List<Peer>>(emptyList())
    override val peers: StateFlow<List<Peer>> = _peers.asStateFlow()
    /** 每台设备还能「从历史里」加载多少条更早的消息。 */
    private val olderBudget = HashMap<String, Int>()

    init {
        if (!empty) seedMessages()
        recompute()
        if (live) scope.launch { tick() }
    }

    // ---------------------------------------------------------------- seed

    private fun seedPeers() = listOf(
        Peer("pc-7f21", "MOTTO-PC", "windows", "192.168.1.8", true, null, 2, null, false),
        Peer("arch-03b9", "motto-arch", "linux", "192.168.1.15", true, "MOTTO-PC", 0, null, false),
        Peer("mac-a810", "MacBook Air", "macos", "192.168.1.31", true, null, 0, null, false),
        Peer("pix-55c2", "小米 14", "android", "192.168.1.42", false, null, 0, null, false),
        Peer("ipad-9e0d", "Lin 的 iPad", "ios", "192.168.1.57", false, null, 0, null, false),
        Peer("tv-11aa", "客厅电视", "android", "192.168.1.90", false, null, 0, null, true),
    )

    private fun m(peer: String, dir: Direction, agoMin: Long, body: Body, delivered: Boolean = true) =
        Message(nextId(peer), peer, dir, now - agoMin * 60_000L, body, delivered)

    private fun seedMessages() {
        val day = 24 * 60L
        val pc = "pc-7f21"
        put(pc, listOf(
            m(pc, Direction.IN, day + 190, Body.Text("晚上把那份设计稿发我一下")),
            m(pc, Direction.OUT, day + 185, Body.Text("好，回家传")),
            m(pc, Direction.OUT, day + 60, Body.File("t-1", "Lancat-设计稿-v3.fig", 48_234_112, "application/octet-stream", "content://fake/doc/1", TransferState.Done)),
            m(pc, Direction.IN, day + 58, Body.Sticker("1f44d")),
            m(pc, Direction.IN, 96, Body.Text("早上好！我在公司电脑上装了新版 Lancat，看看能不能互相发现。")),
            m(pc, Direction.OUT, 95, Body.Text("能看到你了 👌")),
            m(pc, Direction.OUT, 94, Body.Text("顺便测试一下长消息的换行：局域网零配置快传，不经过任何服务器，速度只取决于你家路由器。中英文混排 mixed English text 也要排得整齐，标点不能落在行首。")),
            m(pc, Direction.IN, 80, Body.Sticker("1f602")),
            m(pc, Direction.IN, 42, Body.File("t-2", "IMG_20261016_093012.jpg", 6_512_330, "image/jpeg", imageUri ?: "content://fake/img/2", TransferState.Done)),
            m(pc, Direction.OUT, 30, Body.File("t-3", "会议录像-1016.mp4", 1_288_490_188, "video/mp4", "content://fake/vid/3",
                TransferState.Running(bytes = 512_000_000, total = 1_288_490_188, bytesPerSec = 38_400_000, etaSec = 21))),
            m(pc, Direction.OUT, 26, Body.File("t-4", "照片备份-2026.zip", 3_221_225_472, "application/zip", "content://fake/zip/4",
                TransferState.Paused(bytes = 1_073_741_824, total = 3_221_225_472))),
            m(pc, Direction.IN, 20, Body.File("t-5", "季度报表.xlsx", 2_345_678, null, null, TransferState.Failed("对方断开连接", retryable = true))),
            m(pc, Direction.OUT, 12, Body.File("t-6", "安装包.apk", 98_765_432, "application/vnd.android.package-archive", "content://fake/apk/6", TransferState.Queued)),
            m(pc, Direction.OUT, 10, Body.File("t-7", "旧版本.dmg", 210_000_000, null, null, TransferState.Cancelled)),
            m(pc, Direction.IN, 3, Body.Text("收到了吗？")),
            m(pc, Direction.IN, 2, Body.Text("录像有点大，慢慢传")),
        ))
        olderBudget[pc] = 120

        val arch = "arch-03b9"
        put(arch, listOf(
            m(arch, Direction.OUT, 300, Body.Text("ssh 公钥发你了")),
            m(arch, Direction.IN, 299, Body.Sticker("1f389")),
            m(arch, Direction.IN, 240, Body.Text("dmesg 日志贴一下")),
            m(arch, Direction.OUT, 238, Body.File("t-8", "dmesg.log", 182_331, "text/plain", "content://fake/log/8", TransferState.Done)),
        ))
        val mac = "mac-a810"
        put(mac, listOf(
            m(mac, Direction.IN, 3 * day, Body.Text("周末一起去爬山吗")),
            m(mac, Direction.OUT, 3 * day - 5, Body.Sticker("1f60e")),
        ))
        val pix = "pix-55c2"
        put(pix, listOf(m(pix, Direction.OUT, 9 * day, Body.Text("测试"), delivered = false)))
        put("ipad-9e0d", emptyList())
        put("tv-11aa", listOf(m("tv-11aa", Direction.IN, 40 * day, Body.Text("投屏好了"))))
    }

    private fun put(peer: String, list: List<Message>) {
        store.getOrPut(peer) { MutableStateFlow(emptyList()) }.value = list
    }

    private fun flow(peer: String) = store.getOrPut(peer) { MutableStateFlow(emptyList()) }

    private fun recompute() {
        val list = basePeers.value.map { p ->
            val last = store[p.id]?.value?.lastOrNull()
            p.copy(lastMessage = last?.let { MessagePreview(previewText(it.body), it.timeMs, it.dir == Direction.OUT) })
        }.sortedWith(compareByDescending<Peer> { it.online }.thenByDescending { it.lastMessage?.timeMs ?: 0L })
        _peers.value = list
        allFlow.value = store.values.flatMap { it.value }.sortedBy { it.timeMs }
    }

    private fun previewText(b: Body) = when (b) {
        is Body.Text -> b.text
        is Body.Sticker -> "[贴纸]"
        is Body.File -> "[文件] ${b.name}"
    }

    // ---------------------------------------------------------------- reads

    override fun messages(peerId: String): StateFlow<List<Message>> =
        if (peerId == "*") allFlow.asStateFlow() else flow(peerId).asStateFlow()

    override suspend fun loadOlder(peerId: String, beforeMs: Long, limit: Int): Int {
        val left = olderBudget[peerId] ?: 0
        if (left <= 0 || peerId == "*") return 0
        delay(300)
        val n = minOf(limit, left)
        olderBudget[peerId] = left - n
        val older = (1..n).map { i ->
            val t = beforeMs - i * 37 * 60_000L
            Message(nextId(peerId), peerId, if (i % 3 == 0) Direction.OUT else Direction.IN, t,
                Body.Text("更早的消息 #${left - i + 1}"), true)
        }.reversed()
        flow(peerId).update { older + it }
        recompute()
        return n
    }

    // ---------------------------------------------------------------- writes

    private fun targets(peerIds: List<String>) =
        if (peerIds == listOf("*")) basePeers.value.filter { it.online && !it.blocked }.map { it.id } else peerIds

    private fun append(peer: String, body: Body): Message {
        val msg = Message(nextId(peer), peer, Direction.OUT, System.currentTimeMillis().coerceAtLeast(now), body, delivered = false)
        flow(peer).update { it + msg }
        recompute()
        if (live) scope.launch { delay(400); update(msg.id) { it.copy(delivered = true) } }
        return msg
    }

    override suspend fun sendText(peerIds: List<String>, text: String) { targets(peerIds).forEach { append(it, Body.Text(text)) } }

    override suspend fun sendSticker(peerIds: List<String>, emojiId: String) { targets(peerIds).forEach { append(it, Body.Sticker(emojiId)) } }

    override suspend fun sendFiles(peerIds: List<String>, uris: List<String>) {
        targets(peerIds).forEach { p ->
            uris.forEach { u ->
                val name = u.substringAfterLast('/').substringAfterLast("%2F").ifBlank { "文件" }
                val size = 20_000_000L + (u.hashCode().toLong() and 0xFFFFFF) * 10
                append(p, Body.File(nextId("t"), name, size, null, u, TransferState.Running(0, size, 0, null)))
            }
        }
    }

    private fun update(messageId: String, f: (Message) -> Message) {
        for (fl in store.values) {
            if (fl.value.any { it.id == messageId }) {
                fl.update { l -> l.map { if (it.id == messageId) f(it) else it } }
                recompute(); return
            }
        }
    }

    private fun updateState(messageId: String, f: (TransferState, Body.File) -> TransferState) = update(messageId) { msg ->
        val b = msg.body as? Body.File ?: return@update msg
        val ns = f(b.state, b)
        scope.launch { _events.emit(CoreEvent.TransferProgress(msg.peerId, msg.id, ns)) }
        msg.copy(body = b.copy(state = ns))
    }

    override suspend fun pause(messageId: String) = updateState(messageId) { s, b ->
        if (s is TransferState.Running) TransferState.Paused(s.bytes, s.total) else s
    }

    override suspend fun resume(messageId: String) = updateState(messageId) { s, b ->
        if (s is TransferState.Paused) TransferState.Running(s.bytes, s.total, 0, null) else s
    }

    override suspend fun retry(messageId: String) = updateState(messageId) { _, b -> TransferState.Running(0, b.size, 0, null) }

    override suspend fun cancel(messageId: String) = updateState(messageId) { _, _ -> TransferState.Cancelled }

    override suspend fun deleteMessage(messageId: String) {
        for (fl in store.values) fl.update { l -> l.filterNot { it.id == messageId } }
        recompute()
    }

    override suspend fun markRead(peerId: String) {
        basePeers.update { l -> l.map { if (it.id == peerId || peerId == "*") it.copy(unread = 0) else it } }
        recompute()
    }

    override suspend fun setBlocked(peerId: String, blocked: Boolean) {
        basePeers.update { l -> l.map { if (it.id == peerId) it.copy(blocked = blocked) else it } }
        recompute()
    }

    override suspend fun clearHistory(peerId: String?) {
        if (peerId == null || peerId == "*") store.values.forEach { it.value = emptyList() } else flow(peerId).value = emptyList()
        recompute()
    }

    override suspend fun addManualPeer(address: String) {
        val ip = address.substringBefore(':')
        val id = "manual-" + ip.replace('.', '-')
        if (basePeers.value.none { it.id == id }) {
            basePeers.update { it + Peer(id, ip, "", ip, true, null, 0, null, false) }
            recompute()
        }
    }

    override suspend fun refreshPeers() { delay(900); recompute() }

    override suspend fun typing(peerId: String) {}

    override suspend fun updateSettings(transform: (Settings) -> Settings) { _settings.update(transform) }

    // ---------------------------------------------------------------- live 模拟

    private suspend fun tick() {
        var n = 0
        while (true) {
            delay(500)
            n++
            for (fl in store.values) {
                val l = fl.value
                if (l.none { (it.body as? Body.File)?.state is TransferState.Running }) continue
                fl.value = l.map { msg ->
                    val b = msg.body as? Body.File
                    val s = b?.state as? TransferState.Running ?: return@map msg
                    val rate = 30_000_000L + (n % 7) * 3_000_000L
                    val bytes = (s.bytes + rate / 2).coerceAtMost(s.total)
                    val ns = if (bytes >= s.total) TransferState.Done
                    else TransferState.Running(bytes, s.total, rate, (s.total - bytes) / rate)
                    msg.copy(body = b.copy(state = ns))
                }
            }
            if (n % 30 == 0) {
                val pc = "pc-7f21"
                _events.emit(CoreEvent.PeerTyping(pc))
                delay(2500)
                val msg = Message(nextId(pc), pc, Direction.IN, System.currentTimeMillis(), Body.Text("（自动回复）收到 #${n / 30}"), true)
                flow(pc).update { it + msg }
                basePeers.update { l -> l.map { if (it.id == pc) it.copy(unread = it.unread + 1) else it } }
                _events.emit(CoreEvent.MessageReceived(msg))
            }
            recompute()
        }
    }
}
