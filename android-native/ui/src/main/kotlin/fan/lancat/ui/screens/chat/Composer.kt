package fan.lancat.ui.screens.chat

import android.content.Context
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imeAnimationTarget
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import fan.lancat.ui.components.LancatIcons
import fan.lancat.ui.components.LcIconButton
import fan.lancat.ui.stub.EmojiCatalog
import fan.lancat.ui.stub.EmojiEntry
import fan.lancat.sticker.StickerImage
import fan.lancat.ui.theme.LancatMotion
import fan.lancat.ui.theme.LancatTheme
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

enum class InputMode { NONE, KEYBOARD, EMOJI }

/**
 * Telegram 式键盘 ↔ 表情面板无缝切换。
 *
 * 输入栏下方是一块「底部区」，高度 = max(当前 ime inset, 面板高度, 导航栏)。
 *  - 键盘 → 面板：面板高度瞬间设为上次键盘高度，然后收起键盘；底部区高度始终不变，输入栏不跳。
 *  - 面板 → 键盘：先弹键盘，等 ime inset 长到面板高度再撤面板；同样不跳。
 *  - 面板 ↔ 无：面板高度做 EaseOutQuint 动画。
 * 上次键盘高度存在 SharedPreferences，首次没有时用 300dp。
 *
 * 需要宿主 Activity：enableEdgeToEdge() + windowSoftInputMode=adjustResize（C 组），
 * 并且本页面不能再套 imePadding()。
 */
@Stable
class KeyboardPanelState internal constructor(private val prefs: android.content.SharedPreferences?, density: Density) {
    var mode by mutableStateOf(InputMode.NONE)
        internal set
    /** 上次完整弹出的键盘高度（px，含导航栏）。 */
    var keyboardHeight by mutableFloatStateOf(prefs?.getFloat(KEY_KB, -1f)?.takeIf { it > 0 } ?: with(density) { 300.dp.toPx() })
        private set
    internal val panelHeight = Animatable(0f)

    internal fun rememberKeyboard(px: Float) {
        if (px > 100f && kotlin.math.abs(px - keyboardHeight) > 1f) {
            keyboardHeight = px
            prefs?.edit()?.putFloat(KEY_KB, px)?.apply()
        }
    }

    companion object { const val KEY_KB = "kb_height_px" }
}

@Composable
fun rememberKeyboardPanelState(): KeyboardPanelState {
    val ctx = LocalContext.current
    val density = LocalDensity.current
    return remember { KeyboardPanelState(runCatching { ctx.getSharedPreferences("lancat_ui", Context.MODE_PRIVATE) }.getOrNull(), density) }
}

/** 测试 / 截图用：直接把面板打开到默认高度。 */
suspend fun KeyboardPanelState.debugOpenEmoji() {
    mode = InputMode.EMOJI; panelHeight.snapTo(keyboardHeight)
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun ComposerArea(
    state: KeyboardPanelState,
    placeholder: String,
    enabled: Boolean,
    onSendText: (String) -> Unit,
    onTyping: () -> Unit,
    onAttach: () -> Unit,
    onSticker: (EmojiEntry) -> Unit,
    modifier: Modifier = Modifier,
) {
    val c = LancatTheme.colors
    val density = LocalDensity.current
    val kb = LocalSoftwareKeyboardController.current
    val focus = remember { FocusRequester() }
    val scope = rememberCoroutineScope()
    var text by rememberSaveable { mutableStateOf("") }

    // 只拿 insets 对象，数值在 snapshotFlow / layout 阶段读，键盘动画时不触发整个输入区重组
    val imeInsets = WindowInsets.ime
    val imeTargetInsets = WindowInsets.imeAnimationTarget
    val navInsets = WindowInsets.navigationBars
    fun imeNow(): Float = imeInsets.getBottom(density).toFloat()
    fun navBar(): Float = navInsets.getBottom(density).toFloat()

    // 键盘稳定弹出时记录高度；系统收起键盘时同步模式
    LaunchedEffect(state) {
        snapshotFlow { imeInsets.getBottom(density).toFloat() to imeTargetInsets.getBottom(density).toFloat() }.collect { (now, target) ->
            if (now > 0f && now == target) {
                state.rememberKeyboard(now)
                if (state.mode == InputMode.NONE) state.mode = InputMode.KEYBOARD
            }
            if (target == 0f && state.mode == InputMode.KEYBOARD) state.mode = InputMode.NONE
        }
    }

    fun openEmoji() {
        scope.launch {
            val fromKeyboard = state.mode == InputMode.KEYBOARD || imeNow() > 0f
            state.mode = InputMode.EMOJI
            if (fromKeyboard) {
                state.panelHeight.snapTo(maxOf(state.keyboardHeight, imeNow()))
                kb?.hide()
            } else {
                state.panelHeight.snapTo(navBar())
                state.panelHeight.animateTo(state.keyboardHeight, tween(LancatMotion.PAGE, easing = LancatMotion.EaseOutQuint))
            }
        }
    }

    fun openKeyboard() {
        scope.launch {
            val wasEmoji = state.mode == InputMode.EMOJI
            state.mode = InputMode.KEYBOARD
            focus.requestFocus()
            kb?.show()
            if (wasEmoji) {
                // 等键盘长到面板高度再撤面板
                withTimeoutOrNull(450) { snapshotFlow { imeNow() }.first { it >= state.panelHeight.value - 2f } }
                state.panelHeight.snapTo(0f)
            }
        }
    }

    fun closePanel() {
        scope.launch {
            state.mode = InputMode.NONE
            state.panelHeight.animateTo(0f, tween(LancatMotion.DUR_SLOW, easing = LancatMotion.EaseOutQuint))
        }
    }

    BackHandler(enabled = state.mode == InputMode.EMOJI) { closePanel() }

    Column(modifier.fillMaxWidth().background(c.surface)) {
        // 输入栏
        Row(
            Modifier
                .fillMaxWidth()
                .drawBehind { drawLine(c.sep, Offset(0f, 0f), Offset(size.width, 0f), 1f) }
                .padding(horizontal = 6.dp, vertical = 7.dp),
            verticalAlignment = Alignment.Bottom,
        ) {
            AnimatedContent(
                targetState = state.mode == InputMode.EMOJI,
                transitionSpec = { (scaleIn(initialScale = 0.6f) + fadeIn()) togetherWith (scaleOut(targetScale = 0.6f) + fadeOut()) },
                label = "emojiKb",
            ) { emoji ->
                LcIconButton(
                    if (emoji) LancatIcons.Keyboard else LancatIcons.Smile,
                    if (emoji) "键盘" else "表情",
                    { if (emoji) openKeyboard() else openEmoji() },
                    size = 42.dp, iconSize = 25.dp,
                )
            }
            LcIconButton(LancatIcons.Clip, "附件", onAttach, size = 42.dp, iconSize = 24.dp, enabled = enabled)
            Spacer(Modifier.width(2.dp))
            Box(
                Modifier
                    .weight(1f)
                    .heightIn(min = 42.dp)
                    .background(c.field, RoundedCornerShape(21.dp))
                    .border(0.5.dp, c.sep, RoundedCornerShape(21.dp))
                    .padding(horizontal = 16.dp, vertical = 10.dp)
                    // 面板打开时点输入框：走「面板 → 键盘」流程而不是让系统直接顶起
                    .pointerInput(Unit) {
                        awaitEachGesture {
                            awaitFirstDown(pass = PointerEventPass.Initial)
                            val up = waitForUpOrCancellation(PointerEventPass.Initial)
                            if (up != null && state.mode == InputMode.EMOJI) openKeyboard()
                        }
                    },
                contentAlignment = Alignment.CenterStart,
            ) {
                if (text.isEmpty()) {
                    Text(placeholder, style = LancatTheme.type.body, color = c.text3, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                BasicTextField(
                    value = text,
                    onValueChange = { text = it; onTyping() },
                    enabled = enabled,
                    textStyle = LancatTheme.type.body.copy(color = c.text),
                    cursorBrush = SolidColor(c.accent),
                    maxLines = 6,
                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
                    modifier = Modifier.fillMaxWidth().focusRequester(focus),
                )
            }
            Spacer(Modifier.width(6.dp))
            val canSend = text.isNotBlank() && enabled
            Box(
                Modifier.size(42.dp).background(if (canSend) c.accent else c.accent.copy(alpha = 0.35f), CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                LcIconButton(
                    LancatIcons.Send, "发送",
                    { if (canSend) { onSendText(text.trim()); text = "" } },
                    tint = Color.White, size = 42.dp, iconSize = 22.dp,
                )
            }
        }
        // 底部区：max(ime, 面板, 导航栏)
        val panelVisible by remember { derivedStateOf { state.panelHeight.value > 0.5f } }
        Box(
            Modifier
                .fillMaxWidth()
                .layout { m, cons ->
                    val h = maxOf(imeInsets.getBottom(this).toFloat(), state.panelHeight.value, navInsets.getBottom(this).toFloat()).toInt()
                    val p = m.measure(cons.copy(minHeight = 0, maxHeight = maxOf(state.keyboardHeight.toInt(), h)))
                    layout(cons.maxWidth, h) { p.place(0, 0) }
                }
        ) {
            if (panelVisible) {
                EmojiPanel(
                    heightPx = maxOf(state.keyboardHeight, state.panelHeight.value),
                    bottomInsetPx = navBar(),
                    onPick = { e -> onSticker(e) },
                )
            }
        }
    }
}

// ------------------------------------------------------------------------------------------- 表情面板

private const val PREF_RECENT = "recent_emoji"

/** 最近使用：同桌面，最多存 24 个，显示 21 个。存在 :ui 自己的 SharedPreferences（契约 Settings 里没有这一项）。 */
@Stable
class RecentEmoji(private val prefs: android.content.SharedPreferences?) {
    val codes = mutableStateListOf<String>().apply {
        addAll(prefs?.getString(PREF_RECENT, null)?.split(',')?.filter { it.isNotBlank() } ?: DEFAULT)
    }
    fun push(code: String) {
        codes.remove(code); codes.add(0, code)
        while (codes.size > 24) codes.removeAt(codes.lastIndex)
        prefs?.edit()?.putString(PREF_RECENT, codes.joinToString(","))?.apply()
    }
    companion object { val DEFAULT = listOf("1f602", "1f44d", "2764_fe0f", "1f62d", "1f389", "1f60e", "1f914") }
}

@Composable
fun rememberRecentEmoji(): RecentEmoji {
    val ctx = LocalContext.current
    return remember { RecentEmoji(runCatching { ctx.getSharedPreferences("lancat_ui", Context.MODE_PRIVATE) }.getOrNull()) }
}

private sealed interface GridCell {
    data class Header(val cat: String, val label: String) : GridCell
    data class Cell(val cat: String, val e: EmojiEntry) : GridCell
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun EmojiPanel(heightPx: Float, bottomInsetPx: Float, onPick: (EmojiEntry) -> Unit, modifier: Modifier = Modifier) {
    val c = LancatTheme.colors
    val density = LocalDensity.current
    val recent = rememberRecentEmoji()
    var query by rememberSaveable { mutableStateOf("") }
    val grid = rememberLazyGridState()
    val scope = rememberCoroutineScope()

    val cats = remember { EmojiCatalog.categories.filter { cat -> cat.id == "recent" || EmojiCatalog.all.any { it.cat == cat.id } } }
    val cells: List<GridCell> = remember(recent.codes.toList(), query) {
        if (query.isNotBlank()) {
            EmojiCatalog.search(query).map { GridCell.Cell("search", it) }
        } else buildList {
            val rec = recent.codes.take(21).mapNotNull { EmojiCatalog.byCode(it) }
            if (rec.isNotEmpty()) {
                add(GridCell.Header("recent", "最近使用"))
                rec.forEach { add(GridCell.Cell("recent", it)) }
            }
            for (cat in cats) {
                if (cat.id == "recent") continue
                add(GridCell.Header(cat.id, cat.label))
                EmojiCatalog.all.filter { it.cat == cat.id }.forEach { add(GridCell.Cell(cat.id, it)) }
            }
        }
    }
    val activeCat by remember(cells) {
        derivedStateOf {
            val i = grid.firstVisibleItemIndex
            when (val x = cells.getOrNull(i)) { is GridCell.Header -> x.cat; is GridCell.Cell -> x.cat; null -> "recent" }
        }
    }

    Column(
        modifier
            .fillMaxWidth()
            .height(with(density) { heightPx.toDp() })
            .background(c.surface)
            .padding(bottom = with(density) { bottomInsetPx.toDp() })
    ) {
        // 搜索
        Row(
            Modifier.padding(start = 12.dp, end = 12.dp, top = 8.dp).fillMaxWidth().height(36.dp)
                .background(c.field, RoundedCornerShape(18.dp)).border(0.5.dp, c.sep, RoundedCornerShape(18.dp))
                .padding(horizontal = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(LancatIcons.Search, null, tint = c.text3, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(8.dp))
            Box(Modifier.weight(1f), contentAlignment = Alignment.CenterStart) {
                if (query.isEmpty()) Text("搜索表情", style = LancatTheme.type.bodySmall, color = c.text3)
                BasicTextField(query, { query = it }, singleLine = true, textStyle = LancatTheme.type.bodySmall.copy(color = c.text),
                    cursorBrush = SolidColor(c.accent), modifier = Modifier.fillMaxWidth())
            }
            if (query.isNotEmpty()) LcIconButton(LancatIcons.X, "清除", { query = "" }, size = 28.dp, iconSize = 16.dp)
        }
        // 分类
        if (query.isBlank()) {
            LazyRow(
                Modifier.fillMaxWidth().padding(top = 6.dp).height(40.dp),
                horizontalArrangement = Arrangement.spacedBy(2.dp),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 8.dp),
            ) {
                items(cats, key = { it.id }) { cat ->
                    val on = activeCat == cat.id
                    Box(
                        Modifier.size(40.dp).background(if (on) c.accent.copy(alpha = 0.14f) else Color.Transparent, CircleShape)
                            .clickable {
                                val idx = cells.indexOfFirst { it is GridCell.Header && it.cat == cat.id }
                                if (idx >= 0) scope.launch { grid.animateScrollToItem(idx) }
                            },
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(LancatIcons.get(cat.icon), cat.label, tint = if (on) c.accent else c.text3, modifier = Modifier.size(22.dp))
                    }
                }
            }
        }
        LazyVerticalGrid(
            columns = GridCells.Adaptive(48.dp),
            state = grid,
            modifier = Modifier.fillMaxWidth().weight(1f),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 8.dp, vertical = 4.dp),
        ) {
            if (cells.isEmpty()) {
                item(span = { GridItemSpan(maxLineSpan) }, contentType = "none") {
                    Text("没有找到「$query」", style = LancatTheme.type.bodySmall, color = c.text3,
                        modifier = Modifier.padding(24.dp))
                }
            }
            items(
                cells,
                key = { when (it) { is GridCell.Header -> "h:${it.cat}"; is GridCell.Cell -> "${it.cat}:${it.e.code}" } },
                span = { if (it is GridCell.Header) GridItemSpan(maxLineSpan) else GridItemSpan(1) },
                contentType = { if (it is GridCell.Header) "h" else "e" },
            ) { cell ->
                when (cell) {
                    is GridCell.Header -> Text(
                        cell.label, style = LancatTheme.type.caption, color = c.text3,
                        modifier = Modifier.padding(start = 6.dp, top = 10.dp, bottom = 4.dp),
                    )
                    is GridCell.Cell -> Box(
                        Modifier.size(48.dp).clickable { recent.push(cell.e.code); onPick(cell.e) },
                        contentAlignment = Alignment.Center,
                    ) {
                        StickerImage(emojiId = cell.e.code, size = 36.dp, play = false)
                    }
                }
            }
        }
    }
}

