package fan.lancat.service

import android.content.Context
import android.os.SystemClock
import android.util.Log
import fan.lancat.model.CoreEvent
import fan.lancat.model.LancatRepository
import fan.lancat.model.TransferState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.util.concurrent.ConcurrentHashMap

/**
 * 决定前台服务何时起停：
 * - 有传输（Queued/Running）或正在把分享交给 core（sending）→ 需要服务
 * - 常驻开关打开 → 一直需要
 * - 暂停 / 可重试的失败 → 再保留 10 分钟（服务停了，分享来的 content:// 授权就没了，继续/重试会失败）
 * - 以上都不成立 → LancatService 自己 3 秒后 stopSelf
 */
object ServiceController {
    data class State(val active: Int = 0, val keepAlive: Boolean = false, val sending: Int = 0, val graceUntilMs: Long = 0)

    const val GRACE_MS = 10 * 60 * 1000L
    const val SEND_GRACE_MS = 20 * 1000L

    private val _state = MutableStateFlow(State())
    val state: StateFlow<State> = _state.asStateFlow()
    private val transfers = ConcurrentHashMap<String, TransferState>()
    private lateinit var app: Context
    @Volatile private var lastRenew = 0L

    fun start(ctx: Context, repo: LancatRepository, scope: CoroutineScope) {
        app = ctx.applicationContext
        primeKeepAlive(fan.lancat.SysPrefs.keepAlive(app))
        scope.launch {
            repo.settings.map { it.keepAlive }.distinctUntilChanged().collect { on ->
                _state.update { it.copy(keepAlive = on) }
                if (on) LancatService.ensure(app)
            }
        }
        scope.launch {
            repo.events.collect { e -> if (e is CoreEvent.TransferProgress) onTransfer(e.messageId, e.state) }
        }
        scope.launch {
            state.map { it.active > 0 }.distinctUntilChanged().collect { TransferLocks.set(app, it) }
        }
    }

    fun onTransfer(messageId: String, st: TransferState) {
        val wasEmpty = transfers.isEmpty()
        when (st) {
            is TransferState.Queued, is TransferState.Running -> transfers[messageId] = st
            is TransferState.Paused -> { transfers.remove(messageId); extendGrace(GRACE_MS) }
            is TransferState.Failed -> { transfers.remove(messageId); if (st.retryable) extendGrace(GRACE_MS) }
            else -> transfers.remove(messageId)
        }
        _state.update { it.copy(active = transfers.size) }
        // 唤醒锁带 1 小时超时：长传输每 10 分钟续一次
        val now = SystemClock.elapsedRealtime()
        if (st is TransferState.Running && ::app.isInitialized && now - lastRenew > 10 * 60 * 1000L) {
            lastRenew = now
            TransferLocks.set(app, true)
        }
        if (wasEmpty && transfers.isNotEmpty() && !LancatService.running && ::app.isInitialized) ensureForTransfer(app)
    }

    /** 开机时 core 设置还没加载，先用 SysPrefs 镜像 */
    fun primeKeepAlive(on: Boolean) = _state.update { it.copy(keepAlive = on) }

    fun beginSend() = _state.update { it.copy(sending = it.sending + 1) }
    fun endSend() {
        _state.update { it.copy(sending = (it.sending - 1).coerceAtLeast(0)) }
        extendGrace(SEND_GRACE_MS)
    }

    private fun extendGrace(ms: Long) {
        val until = SystemClock.elapsedRealtime() + ms
        _state.update { if (until > it.graceUntilMs) it.copy(graceUntilMs = until) else it }
    }

    /**
     * 有传输时拉起服务。应用在后台且没开常驻时（比如对方突然发来文件），Android 12+ 不允许从后台启动前台服务，
     * 会抛 ForegroundServiceStartNotAllowedException：此时只发通知、不保活（LancatService.ensure 已吞掉异常并记日志）。
     */
    fun ensureForTransfer(ctx: Context) {
        if (!LancatService.ensure(ctx)) Log.i("lancat", "transfer without foreground service (background start not allowed)")
    }

    internal fun resetForTest() {
        transfers.clear()
        _state.value = State()
    }
}
