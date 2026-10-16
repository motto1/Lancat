package fan.lancat.model

import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.SharedFlow

data class Peer(
    val id: String,            // 设备 id（core 的 node id）
    val name: String,          // 显示名
    val os: String,            // "windows" | "linux" | "macos" | "android" | "ios" | ""
    val ip: String,
    val online: Boolean,
    val relayVia: String?,     // 经某设备中转时为该设备名
    val unread: Int,
    val lastMessage: MessagePreview?,
    val blocked: Boolean,
)

data class MessagePreview(val text: String, val timeMs: Long, val outgoing: Boolean)

enum class Direction { IN, OUT }

sealed interface Body {
    data class Text(val text: String) : Body
    data class Sticker(val emojiId: String) : Body                 // Noto 表情 id，与桌面端相同
    data class File(
        val transferId: String,
        val name: String,
        val size: Long,
        val mime: String?,
        val localUri: String?,     // 收完或发送源的 content:// 或 file://
        val state: TransferState,
    ) : Body
}

sealed interface TransferState {
    data object Queued : TransferState
    data class Running(val bytes: Long, val total: Long, val bytesPerSec: Long, val etaSec: Long?) : TransferState
    data class Paused(val bytes: Long, val total: Long) : TransferState
    data object Done : TransferState
    data class Failed(val reason: String, val retryable: Boolean) : TransferState
    data object Cancelled : TransferState
}

data class Message(
    val id: String,
    val peerId: String,
    val dir: Direction,
    val timeMs: Long,
    val body: Body,
    val delivered: Boolean,
)

data class Settings(
    val deviceName: String,
    val receiveDir: String,        // SAF tree uri，或默认的 Download/Lancat
    val theme: String,             // "system" | "light" | "dark"
    val keepAlive: Boolean,        // 常驻接收（前台服务）
    val autoOpenReceived: Boolean,
    val notifyMessages: Boolean,
)

data class SelfInfo(val id: String, val name: String, val ip: String, val port: Int)

sealed interface CoreEvent {
    data class MessageReceived(val message: Message) : CoreEvent
    data class TransferProgress(val peerId: String, val messageId: String, val state: TransferState) : CoreEvent
    data class PeerTyping(val peerId: String) : CoreEvent
}

/** UI 和系统层唯一可见的接口。:app 用 LancatCore 实现它，:ui 的预览和测试用 FakeLancatRepository。 */
interface LancatRepository {
    val self: StateFlow<SelfInfo?>
    val peers: StateFlow<List<Peer>>              // 已排序：在线优先，其次按最近消息
    val events: SharedFlow<CoreEvent>
    val settings: StateFlow<Settings>

    fun messages(peerId: String): StateFlow<List<Message>>   // 时间升序；"*" 表示全部设备
    suspend fun loadOlder(peerId: String, beforeMs: Long, limit: Int = 50): Int

    suspend fun sendText(peerIds: List<String>, text: String)
    suspend fun sendSticker(peerIds: List<String>, emojiId: String)
    suspend fun sendFiles(peerIds: List<String>, uris: List<String>)   // content:// uri
    suspend fun pause(messageId: String)
    suspend fun resume(messageId: String)
    suspend fun retry(messageId: String)
    suspend fun cancel(messageId: String)
    suspend fun deleteMessage(messageId: String)

    suspend fun markRead(peerId: String)
    suspend fun setBlocked(peerId: String, blocked: Boolean)
    suspend fun clearHistory(peerId: String?)        // null 表示清空全部
    suspend fun addManualPeer(address: String)       // "ip[:port]"
    suspend fun refreshPeers()
    suspend fun typing(peerId: String)

    suspend fun updateSettings(transform: (Settings) -> Settings)
}
