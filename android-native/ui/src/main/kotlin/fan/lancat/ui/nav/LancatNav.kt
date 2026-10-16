package fan.lancat.ui.nav

import androidx.activity.compose.PredictiveBackHandler
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.awaitHorizontalTouchSlopOrCancellation
import androidx.compose.foundation.gestures.horizontalDrag
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.width
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import fan.lancat.ui.theme.LancatMotion
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

/** 单 Activity 内的页面。 */
sealed interface Screen {
    val key: String
    data object Peers : Screen { override val key = "peers" }
    data class Chat(val peerId: String) : Screen { override val key = "chat:$peerId" }
    data object Settings : Screen { override val key = "settings" }
}

/**
 * Compose 自有导航：一个返回栈 + 一条 0..1 的转场进度（1 = 顶层完全可见）。
 * - push：新页从右侧滑入，旧页向左退 25% 并压暗（Telegram ActionBarLayout 的做法）
 * - pop / 系统 predictive back / 全屏右滑：进度跟手，松手按位置和速度决定提交或回弹
 */
@Stable
class Navigator internal constructor(initial: List<Screen>, private val scope: CoroutineScope) {
    val stack = mutableStateListOf<Screen>().apply { addAll(initial) }
    internal val progress = Animatable(1f)
    /** 正在被手势拖动（区别于动画）。 */
    internal var dragging by mutableStateOf(false)
    /** 页面可以临时关掉右滑返回（如长按菜单打开时）。 */
    var swipeBackBlockers by mutableIntStateOf(0)
    private var anim: Job? = null

    val top: Screen get() = stack.last()
    val canPop: Boolean get() = stack.size > 1

    fun push(s: Screen) {
        if (stack.lastOrNull() == s) return
        anim?.cancel()
        stack.removeAll { it == s }
        stack.add(s)
        anim = scope.launch {
            progress.snapTo(0f)
            progress.animateTo(1f, tween(LancatMotion.PAGE, easing = LancatMotion.EaseOutQuint))
        }
    }

    /** 把栈换成 [Peers, s]，用于从通知 / 快捷方式直达。 */
    fun resetTo(vararg s: Screen) {
        anim?.cancel()
        stack.clear(); stack.addAll(s.toList())
        scope.launch { progress.snapTo(1f) }
    }

    fun pop() {
        if (!canPop) return
        anim?.cancel()
        anim = scope.launch { finishPop() }
    }

    internal suspend fun finishPop(velocity: Float = 0f) {
        progress.animateTo(0f, tween(220, easing = LancatMotion.TgDefault), initialVelocity = velocity)
        if (stack.size > 1) stack.removeAt(stack.lastIndex)
        progress.snapTo(1f)
    }

    internal suspend fun cancelPop() {
        progress.animateTo(1f, tween(200, easing = LancatMotion.EaseOutQuint))
    }
}

@Composable
fun rememberNavigator(vararg initial: Screen): Navigator {
    val scope = rememberCoroutineScope()
    return remember { Navigator(initial.toList(), scope) }
}

val LocalNavigator = staticCompositionLocalOf<Navigator?> { null }

@Composable
fun LancatNavHost(nav: Navigator, modifier: Modifier = Modifier, content: @Composable (Screen) -> Unit) {
    val holder = rememberSaveableStateHolder()
    val scope = rememberCoroutineScope()
    var widthPx by remember { mutableIntStateOf(1) }
    val density = LocalDensity.current

    // 系统返回（含 Android 14+ 预测性返回手势）。先于页面内容注册，页面内部的 BackHandler 优先级更高。
    PredictiveBackHandler(enabled = nav.canPop) { events ->
        nav.dragging = true
        try {
            events.collect { e -> nav.progress.snapTo(1f - e.progress.coerceIn(0f, 1f)) }
            nav.dragging = false
            nav.finishPop()
        } catch (c: CancellationException) {
            nav.dragging = false
            scope.launch { nav.cancelPop() }
            throw c
        }
    }

    val transitioning by remember { derivedStateOf { nav.progress.value < 1f || nav.dragging } }
    val stack = nav.stack
    val top = stack.last()
    val below = if (stack.size > 1 && transitioning) stack[stack.size - 2] else null

    Box(modifier.fillMaxSize().onSizeChanged { widthPx = it.width.coerceAtLeast(1) }) {
        if (below != null) {
            key(below.key) {
                Box(Modifier.fillMaxSize().graphicsLayer {
                    translationX = -0.25f * widthPx * nav.progress.value
                }) {
                    holder.SaveableStateProvider(below.key) { content(below) }
                    // 压暗
                    Box(Modifier.fillMaxSize().graphicsLayer { alpha = 0.18f * nav.progress.value }.background(Color.Black))
                }
            }
        }
        key(top.key) {
            Box(
                Modifier
                    .zIndex(1f)
                    .fillMaxSize()
                    .graphicsLayer { translationX = (1f - nav.progress.value) * widthPx }
                    .then(if (stack.size > 1) Modifier.swipeBack(nav, { widthPx }, scope) else Modifier)
            ) {
                holder.SaveableStateProvider(top.key) { content(top) }
                if (below != null) {
                    // 顶层左边缘的投影
                    val shadowW = with(density) { 8.dp.toPx() }
                    Box(
                        Modifier.fillMaxHeight().width(8.dp)
                            .graphicsLayer { translationX = -shadowW; alpha = nav.progress.value.coerceIn(0f, 1f) }
                            .background(Brush.horizontalGradient(listOf(Color.Transparent, Color(0x22000000))))
                    )
                }
            }
        }
    }
}

/** Telegram 式全屏右滑返回：子组件没消费的水平向右拖动都算。 */
private fun Modifier.swipeBack(nav: Navigator, width: () -> Int, scope: CoroutineScope): Modifier = pointerInput(nav) {
    awaitEachGesture {
        val down = awaitFirstDown(requireUnconsumed = false)
        if (nav.swipeBackBlockers > 0) return@awaitEachGesture
        var started = false
        val drag = awaitHorizontalTouchSlopOrCancellation(down.id) { change, over ->
            if (over > 0f) { change.consume(); started = true }
        }
        if (drag == null || !started) return@awaitEachGesture
        nav.dragging = true
        val vt = VelocityTracker()
        vt.addPosition(drag.uptimeMillis, drag.position)
        var dx = (drag.position.x - down.position.x)
        scope.launch { nav.progress.snapTo(1f - dx / width()) }
        val ok = horizontalDrag(drag.id) { ch ->
            dx += ch.positionChange().x
            vt.addPosition(ch.uptimeMillis, ch.position)
            ch.consume()
            val p = (1f - dx / width()).coerceIn(0f, 1f)
            scope.launch { nav.progress.snapTo(p) }
        }
        val v = vt.calculateVelocity().x
        nav.dragging = false
        val frac = dx / width()
        scope.launch {
            if (ok && (frac > 0.35f || (v > 1200f && frac > 0.05f))) nav.finishPop(-v / width()) else nav.cancelPop()
        }
    }
}

private fun androidx.compose.ui.input.pointer.PointerInputChange.positionChange(): Offset = position - previousPosition
