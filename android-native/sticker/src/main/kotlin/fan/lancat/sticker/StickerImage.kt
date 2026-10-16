package fan.lancat.sticker

import android.graphics.Bitmap
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.currentStateAsState
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * 显示一个内置 Noto 动态表情。
 *
 * - `play = false`：只显示首帧静态图（首帧有内存 + 磁盘缓存）；
 * - `play = true`：在屏幕内时从头播 2 遍后停回首帧；鼠标悬停或手指按住时循环播放，松开后把剩下的播完到满 2 遍
 *   （与桌面聊天里的贴纸一致）；
 * - 离开屏幕（不在窗口可见区域 / 从组合中移除 / 页面进入后台）时暂停并释放 native 动画句柄；
 * - 未知的 [emojiId]（不在 [EmojiCatalog] 里）什么都不画，只占位。
 */
@Composable
fun StickerImage(emojiId: String, size: Dp, play: Boolean, modifier: Modifier = Modifier) {
    StickerImage(emojiId = emojiId, size = size, play = play, modifier = modifier, loops = 2)
}

/**
 * 同上，可指定播放遍数：[loops] 遍后停回首帧；`loops <= 0` 表示一直循环（例如表情面板里的预览）。
 */
@Composable
fun StickerImage(emojiId: String, size: Dp, play: Boolean, modifier: Modifier = Modifier, loops: Int = 2) {
    StickerEngine.init(LocalContext.current)
    if (!StickerEngine.nativeOk) {
        GlyphFallback(emojiId, size, modifier)
        return
    }
    val px = with(LocalDensity.current) { size.roundToPx() }.coerceIn(8, StickerConfig.maxPx)
    val player = remember(emojiId, px) { StickerPlayer(emojiId, px) }
    player.loopsWanted = loops

    val lifecycle by LocalLifecycleOwner.current.lifecycle.currentStateAsState()
    val resumed = lifecycle.isAtLeast(Lifecycle.State.STARTED)

    // 首帧
    LaunchedEffect(player) {
        if (player.bitmap == null) {
            StickerEngine.poster(emojiId, px)?.let { if (player.bitmap == null) player.bitmap = it }
        }
    }

    // play 从 false 变成 true（例如新消息进来）时从头计数
    LaunchedEffect(player, play) {
        if (play) player.restart() else player.stopToPoster()
    }

    val active = play && resumed && player.visible && player.wants && !player.bumped
    LaunchedEffect(player, active) {
        if (!active) {
            StickerPlayback.leave(player)
            return@LaunchedEffect
        }
        StickerPlayback.enter(player)
        player.run()
    }

    DisposableEffect(player) {
        onDispose {
            StickerPlayback.leave(player)
            player.dispose()
        }
    }

    val desc = EmojiCatalog.byId(emojiId)?.title
    Spacer(
        modifier
            .size(size)
            .then(if (desc != null) Modifier.semantics { contentDescription = desc } else Modifier)
            .onGloballyPositioned { c ->
                val b = c.boundsInWindow()
                player.updateVisible(c.isAttached && b.width > 0f && b.height > 0f)
            }
            .pointerInput(player) {
                awaitPointerEventScope {
                    while (true) {
                        val ev = awaitPointerEvent(PointerEventPass.Initial) // 不消费，父级的点击 / 长按照常工作
                        when (ev.type) {
                            PointerEventType.Enter -> player.updateHover(true)
                            PointerEventType.Exit -> player.updateHover(false)
                            PointerEventType.Press -> player.updatePressed(true)
                            PointerEventType.Release -> player.updatePressed(false)
                            else -> if (player.pressed && ev.changes.none { it.pressed }) player.updatePressed(false)
                        }
                    }
                }
            }
            .drawBehind {
                val bmp = player.bitmap ?: return@drawBehind
                val dst = IntSize(this.size.width.roundToInt(), this.size.height.roundToInt())
                drawImage(
                    bmp.asImageBitmap(),
                    srcOffset = IntOffset.Zero,
                    srcSize = IntSize(bmp.width, bmp.height),
                    dstOffset = IntOffset.Zero,
                    dstSize = dst,
                    filterQuality = FilterQuality.Medium,
                )
            },
    )
}

/**
 * 一个 StickerImage 实例的播放状态。状态机与桌面 emoji.rs 的 ChatAnim 相同：
 * frame（浮点，按时间推进）、loopsDone、hover、visible、bumped。
 */
@Stable
internal class StickerPlayer(val emojiId: String, val px: Int) {
    var bitmap by mutableStateOf<Bitmap?>(StickerEngine.cachedPoster(emojiId, px))

    var visible by mutableStateOf(false)
        private set
    private var hover by mutableStateOf(false)
    var pressed by mutableStateOf(false)
        private set

    /** 被并发上限挤掉：等下一次 hover / 可见性变化再继续 */
    var bumped by mutableStateOf(false)

    private var loopsDone by mutableStateOf(0)
    var loopsWanted: Int = 2

    private val interacting get() = hover || pressed

    val wants: Boolean get() = interacting || loopsWanted <= 0 || loopsDone < loopsWanted

    private var frame = 0f
    private var disposed = false

    fun restart() {
        frame = 0f
        loopsDone = 0
        bumped = false
    }

    fun stopToPoster() {
        frame = 0f
        StickerEngine.cachedFrame(emojiId, px, 0)?.let { bitmap = it }
    }

    fun updateVisible(v: Boolean) {
        if (v != visible) {
            visible = v
            bumped = false
        }
    }

    fun updateHover(h: Boolean) = setInteract { hover = h }

    fun updatePressed(p: Boolean) = setInteract { pressed = p }

    private inline fun setInteract(change: () -> Unit) {
        val before = interacting
        change()
        val now = interacting
        if (now && !before) {
            // 与桌面 HoverIn 相同：计数清零，从当前帧继续循环
            loopsDone = 0
        }
        if (now != before) bumped = false
    }

    fun dispose() {
        disposed = true
    }

    /** 帧循环（在 LaunchedEffect 里，主线程）：Choreographer 驱动，渲染在后台。 */
    suspend fun run() {
        val handle = withContext(StickerEngine.renderDispatcher) { StickerEngine.acquireHandle(emojiId) } ?: return
        try {
            val total = handle.totalFrames
            if (total < 1f) return
            val fps = handle.fps
            val maxFps = StickerConfig.maxFps.coerceIn(1, 240)
            // 帧号量化：源 60 fps、上限 30 fps 时只渲染偶数帧，重播时能命中帧缓存
            val step = max(1, (fps / maxFps).roundToInt())
            val minIntervalNs = 1_000_000_000L / maxFps - 2_000_000L // 留 2 ms 余量，避免 60 Hz 屏上 30 fps 抖成 20
            var last = withFrameNanos { it }
            var lastShown = Long.MIN_VALUE / 2
            var inflight: Job? = null
            var shownFrame = -1
            while (currentCoroutineContext().isActive && !disposed) {
                val now = withFrameNanos { it }
                val dt = ((now - last) / 1e9f).coerceAtMost(0.2f)
                last = now
                frame += fps * dt
                if (frame >= total) {
                    frame %= total
                    loopsDone++
                    if (!interacting && loopsWanted > 0 && loopsDone >= loopsWanted) {
                        // 播满：停在首帧（= 海报）
                        frame = 0f
                        StickerEngine.cachedFrame(emojiId, px, 0)?.let { bitmap = it }
                            ?: StickerEngine.poster(emojiId, px)?.let { bitmap = it }
                        return
                    }
                }
                if (now - lastShown < minIntervalNs) continue
                val want = ((frame.toInt() / step) * step).coerceIn(0, (total - 1f).toInt().coerceAtLeast(0))
                if (want == shownFrame) continue
                val cached = StickerEngine.cachedFrame(emojiId, px, want)
                if (cached != null) {
                    bitmap = cached
                    shownFrame = want
                    lastShown = now
                    prefetch(handle, want + step, total)
                } else if (inflight?.isActive != true) {
                    // 后台渲染；画好时如果还是要显示的那帧附近就用上（迟到一帧可以接受）
                    inflight = StickerEngine.scope.launch {
                        val b = StickerEngine.renderFrame(handle, emojiId, px, want)
                        if (b != null) prefetchSync(handle, want + step, total)
                    }
                }
            }
        } finally {
            StickerEngine.releaseHandle(emojiId)
        }
    }

    private fun prefetch(handle: LottieAnimationHandle, next: Int, total: Float) {
        val n = if (next >= total.toInt()) 0 else next
        if (StickerEngine.cachedFrame(emojiId, px, n) != null) return
        StickerEngine.scope.launch { StickerEngine.renderFrame(handle, emojiId, px, n) }
    }

    private fun prefetchSync(handle: LottieAnimationHandle, next: Int, total: Float) {
        val n = if (next >= total.toInt()) 0 else next
        StickerEngine.renderFrame(handle, emojiId, px, n)
    }
}

/** 全局播放名单：同时最多 [StickerConfig.maxPlaying] 个，新的挤掉最早开始的（桌面 MAX_PLAYING）。 */
internal object StickerPlayback {
    private val playing = ArrayList<StickerPlayer>()

    fun enter(p: StickerPlayer) {
        val bump: List<StickerPlayer>
        synchronized(playing) {
            playing.remove(p)
            playing.add(p)
            val over = playing.size - StickerConfig.maxPlaying
            bump = if (over > 0) playing.subList(0, over).toList().also { repeat(over) { playing.removeAt(0) } } else emptyList()
        }
        bump.forEach { it.bumped = true }
    }

    fun leave(p: StickerPlayer) {
        synchronized(playing) { playing.remove(p) }
    }

    val count: Int get() = synchronized(playing) { playing.size }
}

/** 编译期检查：三参数调用必须解析到无 loops 的重载（不能有重载歧义）。 */
@Composable
internal fun StickerImageOverloadCheck(id: String, size: Dp) {
    StickerImage(id, size, true)
    StickerImage(id, size, false, Modifier)
    StickerImage(id, size, true, loops = 0)
}

/** 没有 native 渲染器时的兜底：系统 emoji 字形（静态）。 */
@Composable
private fun GlyphFallback(emojiId: String, size: Dp, modifier: Modifier) {
    val ch = EmojiCatalog.byId(emojiId)?.char ?: runCatching {
        emojiId.split('_', '-').joinToString("") { String(Character.toChars(it.toInt(16))) }
    }.getOrDefault("")
    val fs = with(LocalDensity.current) { (size * 0.78f).toSp() }
    androidx.compose.foundation.layout.Box(modifier.size(size), contentAlignment = androidx.compose.ui.Alignment.Center) {
        androidx.compose.foundation.text.BasicText(ch, style = androidx.compose.ui.text.TextStyle(fontSize = fs, lineHeight = fs))
    }
}
