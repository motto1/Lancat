package fan.lancat.stub

import android.content.Context
import android.util.Log
import fan.lancat.model.CoreEvent
import fan.lancat.model.LancatRepository
import fan.lancat.model.Message
import fan.lancat.model.Peer
import fan.lancat.model.SelfInfo
import fan.lancat.model.Settings
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.util.concurrent.ConcurrentHashMap

/**
 * 占位：:core 未就绪时让 :app 能编译、能跑起来。不联网、不发任何东西，只记日志。
 * 签名与契约一致：LancatCoreRepository(context) + onNetworkChanged()。集成时换成 fan.lancat.core.LancatCoreRepository。
 */
class LancatCoreRepository(context: Context) : LancatRepository {
    private val prefs = context.applicationContext.getSharedPreferences("stub_settings", Context.MODE_PRIVATE)

    private val _self = MutableStateFlow<SelfInfo?>(SelfInfo("stub", android.os.Build.MODEL ?: "Android", "0.0.0.0", 0))
    override val self: StateFlow<SelfInfo?> = _self.asStateFlow()
    private val _peers = MutableStateFlow<List<Peer>>(emptyList())
    override val peers: StateFlow<List<Peer>> = _peers.asStateFlow()
    private val _events = MutableSharedFlow<CoreEvent>(extraBufferCapacity = 64)
    override val events: SharedFlow<CoreEvent> = _events.asSharedFlow()
    private val _settings = MutableStateFlow(
        Settings(
            deviceName = android.os.Build.MODEL ?: "Android",
            receiveDir = "Download/Lancat",
            theme = "system",
            keepAlive = prefs.getBoolean("keepAlive", false),
            autoOpenReceived = false,
            notifyMessages = true,
        )
    )
    override val settings: StateFlow<Settings> = _settings.asStateFlow()
    private val msgs = ConcurrentHashMap<String, MutableStateFlow<List<Message>>>()

    var networkChanges = 0
        private set

    fun onNetworkChanged() {
        networkChanges++
        Log.i(TAG, "onNetworkChanged #$networkChanges")
    }

    override fun messages(peerId: String): StateFlow<List<Message>> =
        msgs.getOrPut(peerId) { MutableStateFlow(emptyList()) }

    override suspend fun loadOlder(peerId: String, beforeMs: Long, limit: Int): Int = 0
    override suspend fun sendText(peerIds: List<String>, text: String) { Log.i(TAG, "sendText $peerIds (${text.length} chars)") }
    override suspend fun sendSticker(peerIds: List<String>, emojiId: String) { Log.i(TAG, "sendSticker $peerIds $emojiId") }
    override suspend fun sendFiles(peerIds: List<String>, uris: List<String>) { Log.i(TAG, "sendFiles $peerIds ${uris.size}") }
    override suspend fun pause(messageId: String) {}
    override suspend fun resume(messageId: String) {}
    override suspend fun retry(messageId: String) {}
    override suspend fun cancel(messageId: String) {}
    override suspend fun deleteMessage(messageId: String) {}
    override suspend fun markRead(peerId: String) {
        _peers.update { l -> l.map { if (it.id == peerId) it.copy(unread = 0) else it } }
    }
    override suspend fun setBlocked(peerId: String, blocked: Boolean) {
        _peers.update { l -> l.map { if (it.id == peerId) it.copy(blocked = blocked) else it } }
    }
    override suspend fun clearHistory(peerId: String?) {}
    override suspend fun addManualPeer(address: String) {}
    override suspend fun refreshPeers() {}
    override suspend fun typing(peerId: String) {}
    override suspend fun updateSettings(transform: (Settings) -> Settings) {
        _settings.update(transform)
        prefs.edit().putBoolean("keepAlive", _settings.value.keepAlive).apply()
    }

    private companion object { const val TAG = "lancat-stub" }
}
