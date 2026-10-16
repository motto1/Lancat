package fan.lancat.core

import android.content.ContentResolver
import android.content.Context
import android.media.MediaScannerConnection
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.OpenableColumns
import android.provider.Settings as SysSettings
import android.util.Log
import android.webkit.MimeTypeMap
import fan.lancat.core.ffi.CoreListener
import fan.lancat.core.ffi.FfiConfig
import fan.lancat.core.ffi.FfiEmoji
import fan.lancat.core.ffi.FfiEvent
import fan.lancat.core.ffi.FfiMessage
import fan.lancat.core.ffi.FfiPeer
import fan.lancat.core.ffi.LancatNode
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
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/**
 * The real [LancatRepository]: lancat-core (Rust, same protocol as desktop v0.2.8) via UniFFI.
 *
 * - The node starts on a background thread (it opens the DB and binds TCP/UDP 47820); every call
 *   waits for it. Events arrive on the core's "lancat-events" thread and are funnelled through one
 *   channel into [scope], so all state mutation happens in order on one coroutine.
 * - Files: the core reads plain paths. A content:// uri is resolved to a real readable path when
 *   possible (/proc/self/fd readlink), otherwise copied into filesDir/outbox first.
 * - Received files go to the public Download/Lancat when writable (Android 11+ needs no permission
 *   for files the app creates there; 8–10 use WRITE_EXTERNAL_STORAGE / legacy storage), else the
 *   app's external files dir. Finished files are media-scanned so they show up in Files / Gallery.
 * - Settings: device name / theme / receive dir live in the core (shared with desktop semantics);
 *   keepAlive / autoOpenReceived / notifyMessages are Android-only and live in SharedPreferences.
 */
class LancatCoreRepository(
    context: Context,
    /** tests: change ports / discovery (default = desktop defaults, TCP+UDP 47820, mDNS + UDP on) */
    private val tune: (FfiConfig) -> FfiConfig = { it },
) : LancatRepository {
    private val app = context.applicationContext
    private val prefs = app.getSharedPreferences("lancat_android", Context.MODE_PRIVATE)
    private val serial = Dispatchers.Default.limitedParallelism(1)
    private val scope = CoroutineScope(SupervisorJob() + serial)
    private val main = android.os.Handler(android.os.Looper.getMainLooper())
    /** incoming msgId -> uri after moving into a user-picked SAF folder */
    private val moved = app.getSharedPreferences("lancat_moved", Context.MODE_PRIVATE)
    private val io = Dispatchers.IO

    private val nodeReady = CompletableDeferred<LancatNode>()
    @Volatile private var nodeRef: LancatNode? = null

    private val _self = MutableStateFlow<SelfInfo?>(null)
    override val self: StateFlow<SelfInfo?> = _self.asStateFlow()
    private val _peers = MutableStateFlow<List<Peer>>(emptyList())
    override val peers: StateFlow<List<Peer>> = _peers.asStateFlow()
    private val _events = MutableSharedFlow<CoreEvent>(extraBufferCapacity = 256)
    override val events: SharedFlow<CoreEvent> = _events.asSharedFlow()
    private val _settings = MutableStateFlow(
        Settings(
            deviceName = defaultDeviceName(),
            receiveDir = prefs.getString(K_TREE, "") ?: "",
            theme = "system",
            keepAlive = prefs.getBoolean(K_KEEP, false),
            autoOpenReceived = prefs.getBoolean(K_AUTO_OPEN, false),
            notifyMessages = prefs.getBoolean(K_NOTIFY, true),
        )
    )
    override val settings: StateFlow<Settings> = _settings.asStateFlow()

    /** peerId -> loaded conversation (ascending). "*" is the merged all-devices view. */
    private val convs = ConcurrentHashMap<String, MutableStateFlow<List<Message>>>()
    private val loaded = ConcurrentHashMap.newKeySet<String>()
    /** msgId -> peerId, for pause / retry by message id */
    private val owner = ConcurrentHashMap<String, String>()
    /** live progress: msgId -> (bytes, total, speed sample) */
    private val progress = ConcurrentHashMap<String, Prog>()
    private val inbox = Channel<FfiEvent>(Channel.UNLIMITED)
    private val lastPeerOutgoing = ConcurrentHashMap<String, Boolean>()

    private class Prog(var bytes: Long, var total: Long, var t0: Long, var b0: Long, var bps: Long)

    init {
        scope.launch { for (ev in inbox) runCatching { handle(ev) }.onFailure { Log.w(TAG, "event $ev: $it") } }
        Thread({ startNode() }, "lancat-start").start()
    }

    // ------------------------------------------------------------------ startup

    private fun startNode() {
        try {
            val data = File(app.filesDir, "lancat").apply { mkdirs() }
            val recv = pickReceiveDir()
            val listener = object : CoreListener {
                override fun onEvent(event: FfiEvent) {
                    inbox.trySend(event)
                }
            }
            val node = LancatNode.start(
                tune(FfiConfig(dataDir = data.absolutePath, deviceName = null, receiveDir = recv.absolutePath)),
                listener,
            )
            nodeRef = node
            // a fresh install: the core would announce "localhost"; use the phone's own name
            val s = node.settings()
            if (s.deviceName.isBlank()) runCatching { node.setSettings(s.copy(deviceName = defaultDeviceName())) }
            nodeReady.complete(node)
            scope.launch {
                refreshSelf(node)
                syncSettings(node.settings())
                reloadPeers(node)
            }
            Log.i(TAG, "core ${fan.lancat.core.ffi.coreVersion()} started on port ${node.port()} recv=${node.receiveDir()}")
        } catch (e: Throwable) {
            Log.e(TAG, "core start failed", e)
            nodeReady.completeExceptionally(e)
        }
    }

    private suspend fun node(): LancatNode = nodeRef ?: nodeReady.await()

    private fun pickReceiveDir(): File {
        @Suppress("DEPRECATION")
        val pub = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "Lancat")
        if (writable(pub)) return pub
        val own = app.getExternalFilesDir("Lancat") ?: File(app.filesDir, "Received")
        own.mkdirs()
        return own
    }

    private fun writable(dir: File): Boolean = try {
        dir.mkdirs()
        val probe = File(dir, ".lancat-probe-${UUID.randomUUID()}")
        probe.writeText("ok")
        probe.delete()
        true
    } catch (e: Exception) {
        false
    }

    private fun defaultDeviceName(): String {
        val n = runCatching { SysSettings.Global.getString(app.contentResolver, SysSettings.Global.DEVICE_NAME) }.getOrNull()
        return n?.takeIf { it.isNotBlank() } ?: listOfNotNull(Build.MANUFACTURER?.replaceFirstChar { it.uppercase() }, Build.MODEL)
            .distinct().joinToString(" ").ifBlank { "Android" }
    }

    /** Wi-Fi changes: re-probe now instead of waiting for the core's interface poll. */
    fun onNetworkChanged() {
        nodeRef?.let { n ->
            runCatching { n.onNetworkChanged() }
            scope.launch { refreshSelf(n) }
        }
    }

    // ------------------------------------------------------------------ mapping

    private fun refreshSelf(n: LancatNode) {
        val me = n.me()
        _self.value = SelfInfo(me.id, me.name, me.ip, n.port().toInt())
    }

    private fun syncSettings(s: fan.lancat.core.ffi.FfiSettings) {
        _settings.update {
            it.copy(
                deviceName = s.deviceName.ifBlank { _self.value?.name ?: it.deviceName },
                theme = s.theme.ifBlank { "system" },
            )
        }
    }

    private fun reloadPeers(n: LancatNode) {
        _peers.value = n.peers().map(::peer)
    }

    private fun peer(p: FfiPeer) = Peer(
        id = p.id,
        name = p.name.ifBlank { p.addr },
        os = p.os,
        ip = p.addr,
        online = p.online,
        relayVia = p.via,
        unread = p.unread.toInt(),
        lastMessage = if (p.lastTs > 0) MessagePreview(p.lastPreview, p.lastTs, lastPeerOutgoing[p.id] ?: false) else null,
        blocked = p.blocked,
    )

    private fun message(m: FfiMessage): Message {
        owner[m.id] = m.peerId
        val body: Body = when (m.kind) {
            "text" -> Body.Text(m.text ?: "")
            "emoji" -> {
                val e = m.emoji
                if (e != null && e.kind == "noto") Body.Sticker(e.code) else Body.Text(e?.alt?.ifBlank { null } ?: "[贴纸]")
            }
            "file" -> {
                val f = m.file
                val name = f?.name ?: "file"
                val path = f?.path.orEmpty()
                val local = moved.getString(m.id, null) ?: when {
                    path.startsWith("content://") || path.startsWith("file://") -> path
                    path.isNotEmpty() && (m.outgoing || m.status == "done") && File(path).exists() -> Uri.fromFile(File(path)).toString()
                    else -> null
                }
                Body.File(
                    transferId = f?.transferId ?: m.id,
                    name = name,
                    size = f?.size ?: 0L,
                    mime = mimeOf(name),
                    localUri = local,
                    state = state(m),
                )
            }
            else -> Body.Text(m.text ?: "")
        }
        return Message(
            id = m.id,
            peerId = m.peerId,
            dir = if (m.outgoing) Direction.OUT else Direction.IN,
            timeMs = m.ts,
            body = body,
            delivered = !m.outgoing || m.status !in setOf("pending", "failed"),
        )
    }

    private fun state(m: FfiMessage): TransferState {
        val size = m.file?.size ?: 0L
        val p = progress[m.id]
        return when (m.status) {
            "done" -> TransferState.Done
            "cancelled" -> TransferState.Cancelled
            "failed" -> TransferState.Failed(m.file?.error ?: "传输失败", retryable(m))
            "paused" -> TransferState.Paused(p?.bytes ?: 0L, p?.total ?: size)
            "transferring", "receiving" -> running(p, size)
            "pending", "sent", "delivered", "received" -> if (p != null && p.bytes > 0) running(p, size) else TransferState.Queued
            else -> TransferState.Queued
        }
    }

    private fun running(p: Prog?, size: Long): TransferState.Running {
        val total = p?.total?.takeIf { it > 0 } ?: size
        val bytes = p?.bytes ?: 0L
        val bps = p?.bps ?: 0L
        val eta = if (bps > 0 && total > bytes) (total - bytes) / bps else null
        return TransferState.Running(bytes, total, bps, eta)
    }

    /** Only outgoing transfers can be retried from this side (the core resumes where the receiver left off). */
    private fun retryable(m: FfiMessage): Boolean =
        m.outgoing && m.file?.code !in setOf("peer_too_large", "peer_rejected", "source_missing", "peer_outdated")

    private fun mimeOf(name: String): String? =
        MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase())

    // ------------------------------------------------------------------ conversations

    private fun conv(peerId: String): MutableStateFlow<List<Message>> =
        convs.getOrPut(peerId) { MutableStateFlow(emptyList()) }

    override fun messages(peerId: String): StateFlow<List<Message>> {
        val f = conv(peerId)
        if (loaded.add(peerId)) scope.launch { initialLoad(peerId) }
        return f.asStateFlow()
    }

    private suspend fun initialLoad(peerId: String) {
        val n = node()
        if (peerId == ALL) {
            rebuildAll(n)
            return
        }
        val list = withContext(io) { runCatching { n.messages(peerId, null, PAGE.toUInt()) }.getOrDefault(emptyList()) }
        val mapped = list.map(::message)
        // keep anything an event delivered while we were loading
        conv(peerId).update { cur -> merge(mapped, cur) }
        mapped.lastOrNull()?.let { lastPeerOutgoing[peerId] = it.dir == Direction.OUT }
    }

    private suspend fun rebuildAll(n: LancatNode) {
        val ps = _peers.value.ifEmpty { n.peers().map(::peer) }
        val all = withContext(io) {
            ps.flatMap { p -> runCatching { n.messages(p.id, null, 30u) }.getOrDefault(emptyList()) }
        }.map(::message)
        conv(ALL).value = all.sortedBy { it.timeMs }
    }

    private fun merge(a: List<Message>, b: List<Message>): List<Message> {
        val byId = LinkedHashMap<String, Message>()
        a.forEach { byId[it.id] = it }
        b.forEach { byId[it.id] = it }
        return byId.values.sortedBy { it.timeMs }
    }

    override suspend fun loadOlder(peerId: String, beforeMs: Long, limit: Int): Int {
        if (peerId == ALL) return 0
        val n = node()
        val older = withContext(io) { n.messages(peerId, beforeMs, limit.coerceAtLeast(1).toUInt()) }.map(::message)
        if (older.isEmpty()) return 0
        withContext(serial) { conv(peerId).update { merge(older, it) } }
        return older.size
    }

    private fun upsert(m: Message) {
        val upd: (List<Message>) -> List<Message> = { list ->
            val i = list.indexOfFirst { it.id == m.id }
            when {
                i >= 0 -> list.toMutableList().also { it[i] = m }
                list.isEmpty() || list.last().timeMs <= m.timeMs -> list + m
                else -> (list + m).sortedBy { it.timeMs }
            }
        }
        conv(m.peerId).update(upd)
        convs[ALL]?.update(upd)
    }

    private fun mutate(msgId: String, f: (Message) -> Message) {
        val pid = owner[msgId] ?: return
        for (key in listOf(pid, ALL)) convs[key]?.update { l -> l.map { if (it.id == msgId) f(it) else it } }
    }

    private fun find(msgId: String): Message? {
        val pid = owner[msgId] ?: return null
        return convs[pid]?.value?.firstOrNull { it.id == msgId }
    }

    // ------------------------------------------------------------------ events

    private suspend fun handle(ev: FfiEvent) {
        when (ev) {
            is FfiEvent.Peer -> upsertPeer(peer(ev.peer))
            is FfiEvent.PeerRemoved -> _peers.update { l -> l.filterNot { it.id == ev.peerId } }
            is FfiEvent.Message -> {
                val m = message(ev.message)
                lastPeerOutgoing[m.peerId] = m.dir == Direction.OUT
                upsert(m)
                if (m.dir == Direction.IN) {
                    if (m.body is Body.File) scanIfDone(ev.message)
                    _events.emit(CoreEvent.MessageReceived(m))
                }
                (m.body as? Body.File)?.let { _events.emit(CoreEvent.TransferProgress(m.peerId, m.id, it.state)) }
            }
            is FfiEvent.MessageUpdate -> {
                val m = message(ev.message)
                val before = find(m.id)
                upsert(m)
                val b = m.body as? Body.File
                if (b != null) {
                    val terminal = b.state is TransferState.Done || b.state is TransferState.Failed || b.state is TransferState.Cancelled
                    if (terminal) progress.remove(m.id)
                    if (b.state is TransferState.Done) scanIfDone(ev.message)
                    val prevState = (before?.body as? Body.File)?.state
                    if (prevState == null || prevState::class != b.state::class || terminal) {
                        _events.emit(CoreEvent.TransferProgress(m.peerId, m.id, b.state))
                    }
                }
            }
            is FfiEvent.Progress -> {
                owner[ev.msgId] = ev.peerId
                val now = System.nanoTime()
                val p = progress.getOrPut(ev.msgId) { Prog(ev.done, ev.total, now, ev.done, 0) }
                p.bytes = ev.done
                p.total = ev.total
                val dt = now - p.t0
                if (dt >= 700_000_000L) {
                    val inst = ((ev.done - p.b0).coerceAtLeast(0) * 1_000_000_000L) / dt
                    p.bps = if (p.bps == 0L) inst else (p.bps * 6 + inst * 4) / 10
                    p.t0 = now
                    p.b0 = ev.done
                }
                val st = running(p, ev.total)
                mutate(ev.msgId) { msg ->
                    val b = msg.body as? Body.File ?: return@mutate msg
                    if (b.state is TransferState.Done || b.state is TransferState.Cancelled) msg else msg.copy(body = b.copy(state = st))
                }
                _events.emit(CoreEvent.TransferProgress(ev.peerId, ev.msgId, st))
            }
            is FfiEvent.Typing -> if (ev.typing) _events.emit(CoreEvent.PeerTyping(ev.peerId))
            is FfiEvent.HistoryCleared -> {
                val pid = ev.peerId
                if (pid == null) convs.values.forEach { it.value = emptyList() } else {
                    convs[pid]?.value = emptyList()
                    convs[ALL]?.update { l -> l.filterNot { it.peerId == pid } }
                }
                nodeRef?.let(::reloadPeers)
            }
            is FfiEvent.Settings -> {
                syncSettings(ev.settings)
                nodeRef?.let(::refreshSelf)
            }
            is FfiEvent.Alert -> Log.w(TAG, "core alert ${ev.kind}: ${ev.detail}")
            is FfiEvent.Lagged -> {
                val n = nodeRef ?: return
                reloadPeers(n)
                for (pid in loaded) {
                    if (pid == ALL) rebuildAll(n) else {
                        val list = withContext(io) { runCatching { n.messages(pid, null, PAGE.toUInt()) }.getOrDefault(emptyList()) }
                        conv(pid).value = list.map(::message)
                    }
                }
            }
        }
    }

    private fun upsertPeer(p: Peer) {
        _peers.update { l ->
            val i = l.indexOfFirst { it.id == p.id }
            val next = if (i >= 0) l.toMutableList().also { it[i] = p } else l + p
            next.sortedWith(compareByDescending<Peer> { it.online }.thenByDescending { it.lastMessage?.timeMs ?: 0L }.thenBy { it.name })
        }
    }

    /** A received file is complete: move it into the user's SAF folder if one is set, else media-scan it. */
    private suspend fun scanIfDone(m: FfiMessage) {
        val path = m.file?.path ?: return
        if (m.outgoing || m.status != "done" || path.isEmpty() || moved.contains(m.id)) return
        val f = File(path)
        if (!f.exists()) return
        val tree = _settings.value.receiveDir
        if (tree.startsWith("content://")) {
            val doc = withContext(io) { runCatching { copyToTree(Uri.parse(tree), f) }.onFailure { Log.w(TAG, "move to $tree: $it") }.getOrNull() }
            if (doc != null) {
                moved.edit().putString(m.id, doc.toString()).apply()
                f.delete()
                mutate(m.id) { msg -> (msg.body as? Body.File)?.let { msg.copy(body = it.copy(localUri = doc.toString())) } ?: msg }
                return
            }
        }
        runCatching { MediaScannerConnection.scanFile(app, arrayOf(path), null, null) }
    }

    private fun copyToTree(tree: Uri, f: File): Uri {
        val cr = app.contentResolver
        val parent = android.provider.DocumentsContract.buildDocumentUriUsingTree(tree, android.provider.DocumentsContract.getTreeDocumentId(tree))
        val doc = android.provider.DocumentsContract.createDocument(cr, parent, mimeOf(f.name) ?: "application/octet-stream", f.name)
            ?: error("createDocument failed")
        try {
            (cr.openOutputStream(doc, "w") ?: error("openOutputStream failed")).use { out -> f.inputStream().use { it.copyTo(out, 1 shl 18) } }
        } catch (e: Exception) {
            runCatching { android.provider.DocumentsContract.deleteDocument(cr, doc) }
            throw e
        }
        return doc
    }

    /** Commands never throw into the UI's coroutine scopes (that would crash the app): log + toast instead. */
    private suspend inline fun safe(what: String, crossinline block: suspend () -> Unit) {
        try {
            block()
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            Log.w(TAG, "$what failed", e)
            val msg = (e.message ?: e.toString()).take(120)
            main.post { android.widget.Toast.makeText(app, "${what}失败：$msg", android.widget.Toast.LENGTH_SHORT).show() }
        }
    }

    // ------------------------------------------------------------------ commands

    private fun targets(peerIds: List<String>): List<String> =
        if (peerIds.contains(ALL)) _peers.value.filter { it.online && !it.blocked }.map { it.id } else peerIds

    override suspend fun sendText(peerIds: List<String>, text: String) { safe("发送") {
        val t = targets(peerIds)
        if (t.isEmpty() || text.isBlank()) return@safe
        node().sendText(t, text)
    } }

    override suspend fun sendSticker(peerIds: List<String>, emojiId: String) { safe("发送表情") {
        val t = targets(peerIds)
        if (t.isEmpty()) return@safe
        node().sendEmoji(t, FfiEmoji(kind = "noto", code = emojiId, alt = emojiChar(emojiId), data = null))
    } }

    override suspend fun sendFiles(peerIds: List<String>, uris: List<String>) { safe("发送文件") {
        val t = targets(peerIds)
        if (t.isEmpty() || uris.isEmpty()) return@safe
        val paths = withContext(io) { uris.mapNotNull { runCatching { toPath(it) }.onFailure { e -> Log.w(TAG, "file $it: $e") }.getOrNull() } }
        if (paths.isEmpty()) throw IllegalArgumentException("读取不到要发送的文件")
        node().sendFiles(t, paths)
    } }

    /** content:// / file:// / plain path -> a filesystem path the core can open (and reopen on retry). */
    private fun toPath(s: String): String {
        if (s.startsWith("file:")) return File(java.net.URI(s)).path
        if (!s.contains("://")) return s
        val u = Uri.parse(s)
        val cr = app.contentResolver
        // a real file behind the provider that we may read directly (our own FileProvider, many file managers)
        runCatching {
            cr.openFileDescriptor(u, "r")?.use { pfd ->
                val real = File("/proc/self/fd/${pfd.fd}").canonicalFile
                if (real.isFile && real.canRead() && !real.path.startsWith("/proc") && real.name == displayName(cr, u)) return real.path
            }
        }
        val name = sanitize(displayName(cr, u) ?: u.lastPathSegment ?: "file")
        val dir = File(app.filesDir, "outbox/${UUID.randomUUID()}").apply { mkdirs() }
        val out = File(dir, name)
        (cr.openInputStream(u) ?: error("无法读取 $s")).use { input -> out.outputStream().use { input.copyTo(it, 1 shl 18) } }
        return out.path
    }

    private fun displayName(cr: ContentResolver, u: Uri): String? = runCatching {
        cr.query(u, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst() && !c.isNull(0)) c.getString(0) else null
        }
    }.getOrNull()

    private fun sanitize(n: String): String =
        n.replace(Regex("[\\\\/:*?\"<>|\\u0000-\\u001f]"), "_").trim().trimStart('.').ifEmpty { "file" }.take(180)

    private fun emojiChar(id: String): String = runCatching {
        id.split('_', '-').joinToString("") { String(Character.toChars(it.toInt(16))) }
    }.getOrDefault("")

    override suspend fun pause(messageId: String) { safe("暂停") {
        withContext(io) { node().pauseTransfer(messageId) }
    } }

    override suspend fun resume(messageId: String) = retry(messageId)

    override suspend fun retry(messageId: String) { safe("重试") {
        val m = find(messageId)
        if (m != null && m.dir == Direction.IN) {
            // the receiver cannot pull; the sender resumes from where we left off when it retries
            throw IllegalStateException("请在发送方重试，会从断点继续")
        }
        node().retryTransfer(messageId)
    } }

    override suspend fun cancel(messageId: String) { safe("取消") {
        withContext(io) { node().cancelTransfer(messageId) }
    } }

    override suspend fun deleteMessage(messageId: String) { safe("删除") {
        withContext(io) { node().deleteMessage(messageId) }
        val pid = owner.remove(messageId)
        withContext(serial) {
            for (key in listOfNotNull(pid, ALL)) convs[key]?.update { l -> l.filterNot { it.id == messageId } }
        }
    } }

    override suspend fun markRead(peerId: String) { safe("标记已读") {
        val n = node()
        val ids = if (peerId == ALL) _peers.value.filter { it.unread > 0 }.map { it.id } else listOf(peerId)
        withContext(io) { ids.forEach { runCatching { n.markRead(it) } } }
        withContext(serial) { _peers.update { l -> l.map { if (it.id in ids) it.copy(unread = 0) else it } } }
    } }

    override suspend fun setBlocked(peerId: String, blocked: Boolean) { safe("拉黑") {
        withContext(io) { node().setBlocked(peerId, blocked) }
    } }

    override suspend fun clearHistory(peerId: String?) { safe("清空记录") {
        withContext(io) { node().clearHistory(peerId?.takeIf { it != ALL }) }
    } }

    override suspend fun addManualPeer(address: String) {
        val a = address.trim()
        if (a.isEmpty()) return
        val n = node()
        val p = n.addPeer(a)
        // remember it like the desktop's manual peer list, so it is probed again after restarts
        withContext(io) {
            val s = n.settings()
            if (a !in s.manualPeers) runCatching { n.setSettings(s.copy(manualPeers = s.manualPeers + a)) }
        }
        withContext(serial) { upsertPeer(peer(p)) }
    }

    override suspend fun refreshPeers() { safe("刷新") {
        val n = node()
        withContext(io) { n.refreshPeers() }
        withContext(serial) { refreshSelf(n) }
    } }

    override suspend fun typing(peerId: String) { runCatching {
        if (peerId == ALL) return
        withContext(io) { node().sendTyping(peerId, true) }
    } }

    override suspend fun updateSettings(transform: (Settings) -> Settings) {
        val before = _settings.value
        val next = transform(before)
        prefs.edit()
            .putBoolean(K_KEEP, next.keepAlive)
            .putBoolean(K_AUTO_OPEN, next.autoOpenReceived)
            .putBoolean(K_NOTIFY, next.notifyMessages)
            .putString(K_TREE, next.receiveDir.takeIf { it.startsWith("content://") } ?: "")
            .apply()
        _settings.value = next.copy(receiveDir = next.receiveDir.takeIf { it.startsWith("content://") } ?: "")
        if (next.deviceName != before.deviceName || next.theme != before.theme) {
            val n = node()
            safe("保存设置") {
                withContext(io) {
                    val s = n.settings()
                    n.setSettings(s.copy(deviceName = next.deviceName.trim(), theme = next.theme))
                }
            }
        }
    }

    /** App really exits (not used on normal backgrounding). */
    fun shutdown() {
        nodeRef?.shutdown()
    }

    companion object {
        private const val TAG = "lancat"
        const val ALL = "*"
        private const val PAGE = 60
        private const val K_KEEP = "keep_alive"
        private const val K_AUTO_OPEN = "auto_open"
        private const val K_NOTIFY = "notify_messages"
        private const val K_TREE = "receive_tree"
    }
}
