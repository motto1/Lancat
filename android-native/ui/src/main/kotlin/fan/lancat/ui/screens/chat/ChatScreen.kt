package fan.lancat.ui.screens.chat

import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fan.lancat.model.Body
import fan.lancat.model.CoreEvent
import fan.lancat.model.Direction
import fan.lancat.model.LancatRepository
import fan.lancat.model.Message
import fan.lancat.model.Peer
import fan.lancat.model.TransferState
import fan.lancat.ui.components.AllAvatar
import fan.lancat.ui.components.LancatIcons
import fan.lancat.ui.components.LcIconButton
import fan.lancat.ui.components.LcTopBar
import fan.lancat.ui.components.LocalFlash
import fan.lancat.ui.components.PeerAvatar
import fan.lancat.ui.components.copyToClipboard
import fan.lancat.ui.nav.LocalNavigator
import fan.lancat.ui.theme.AvatarColors
import fan.lancat.ui.theme.LancatMotion
import fan.lancat.ui.theme.LancatShapes
import fan.lancat.ui.theme.LancatTheme
import fan.lancat.ui.util.Fmt
import fan.lancat.ui.util.LocalClock
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.launch
import kotlin.math.roundToInt

/** 截图测试用的初始状态。 */
@Immutable
data class ChatDebug(
    val emojiOpen: Boolean = false,
    val menuOnMessageIndexFromEnd: Int? = null,
    val selectIndicesFromEnd: List<Int> = emptyList(),
    val typing: Boolean = false,
    val attachOpen: Boolean = false,
)

/** 聊天列表里的一项（反向布局，index 0 是最新）。 */
@Immutable
sealed interface ChatItem {
    val key: String
    data class Msg(
        val m: Message,
        val joinTop: Boolean,
        val joinBottom: Boolean,
        val showSender: Boolean,
    ) : ChatItem { override val key get() = m.id }
    data class Day(val dayStart: Long) : ChatItem { override val key get() = "d:$dayStart" }
}

private const val GROUP_GAP_MS = 5 * 60_000L

internal fun buildItems(list: List<Message>, all: Boolean): List<ChatItem> {
    val out = ArrayList<ChatItem>(list.size + 8)
    var lastDay = Long.MIN_VALUE
    for (i in list.indices) {
        val m = list[i]
        val day = Fmt.startOfDay(m.timeMs)
        if (day != lastDay) { out.add(ChatItem.Day(day)); lastDay = day }
        val prev = list.getOrNull(i - 1)
        val next = list.getOrNull(i + 1)
        fun same(a: Message?, b: Message) = a != null && a.dir == b.dir && a.peerId == b.peerId &&
            kotlin.math.abs(a.timeMs - b.timeMs) < GROUP_GAP_MS && Fmt.startOfDay(a.timeMs) == Fmt.startOfDay(b.timeMs)
        val joinTop = same(prev, m)
        out.add(ChatItem.Msg(m, joinTop = joinTop, joinBottom = same(next, m), showSender = all && !joinTop))
    }
    out.reverse()
    return out
}

private fun contentTypeOf(it: ChatItem): String = when (it) {
    is ChatItem.Day -> "day"
    is ChatItem.Msg -> when (val b = it.m.body) {
        is Body.Text -> if (it.m.dir == Direction.OUT) "text-out" else "text-in"
        is Body.Sticker -> "sticker"
        is Body.File -> "file"
    }
}

private data class MenuTarget(val item: ChatItem.Msg, val bounds: Rect)

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun ChatScreen(
    repo: LancatRepository,
    peerId: String,
    onBack: () -> Unit,
    debug: ChatDebug = ChatDebug(),
) {
    val c = LancatTheme.colors
    val ctx = LocalContext.current
    val flash = LocalFlash.current
    val scope = rememberCoroutineScope()
    val nav = LocalNavigator.current
    val isAll = peerId == "*"

    val peers by repo.peers.collectAsStateWithLifecycle()
    val peer: Peer? = remember(peers, peerId) { peers.firstOrNull { it.id == peerId } }
    val messages by remember(peerId) { repo.messages(peerId) }.collectAsStateWithLifecycle()
    val items = remember(messages, isAll) { buildItems(messages, isAll) }
    val peerNames = remember(peers) { peers.associate { it.id to it.name } }

    // ---- 新消息入场：只给打开页面之后到达的消息播放
    val seen = remember(peerId) { HashSet<String>() }
    var openedNewest by remember(peerId) { mutableLongStateOf(Long.MIN_VALUE) }
    if (openedNewest == Long.MIN_VALUE && messages.isNotEmpty()) {
        openedNewest = messages.last().timeMs
        messages.forEach { seen.add(it.id) }
    }

    // ---- 正在输入
    var typingUntil by remember(peerId) { mutableLongStateOf(if (debug.typing) Long.MAX_VALUE else 0L) }
    LaunchedEffect(peerId) {
        repo.events.collect { e ->
            when (e) {
                is CoreEvent.PeerTyping -> if (e.peerId == peerId || isAll) typingUntil = System.currentTimeMillis() + 5_000
                is CoreEvent.MessageReceived -> if (e.message.peerId == peerId) typingUntil = 0
                else -> {}
            }
        }
    }
    LaunchedEffect(typingUntil) {
        val left = typingUntil - System.currentTimeMillis()
        if (left in 1..60_000) { delay(left); typingUntil = 0 }
    }
    // ---- 已读
    LaunchedEffect(peerId, messages.size) { repo.markRead(peerId) }

    val listState = rememberLazyListState()
    // 到底部时来新消息 / 自己发了消息 → 滚到底
    LaunchedEffect(peerId) {
        snapshotFlow { messages.lastOrNull()?.id }.distinctUntilChanged().collect {
            val last = messages.lastOrNull() ?: return@collect
            if (listState.firstVisibleItemIndex <= 2 || last.dir == Direction.OUT) listState.animateScrollToItem(0)
        }
    }
    // 上拉加载更早
    var loadingOlder by remember { mutableStateOf(false) }
    var noMore by remember(peerId) { mutableStateOf(false) }
    LaunchedEffect(peerId) {
        snapshotFlow {
            val info = listState.layoutInfo
            (info.visibleItemsInfo.lastOrNull()?.index ?: 0) >= info.totalItemsCount - 6 && info.totalItemsCount > 0
        }.distinctUntilChanged().filter { it }.collect {
            if (loadingOlder || noMore) return@collect
            val oldest = messages.firstOrNull()?.timeMs ?: return@collect
            loadingOlder = true
            try { if (repo.loadOlder(peerId, oldest) == 0) noMore = true } finally { loadingOlder = false }
        }
    }

    // ---- 多选
    val selection = remember(peerId) { mutableStateMapOf<String, Boolean>() }
    val selecting by remember { derivedStateOf { selection.isNotEmpty() } }
    BackHandler(enabled = selecting) { selection.clear() }

    // ---- 长按菜单
    var menu by remember { mutableStateOf<MenuTarget?>(null) }
    BackHandler(enabled = menu != null) { menu = null }
    LaunchedEffect(menu != null) {
        if (nav != null) { if (menu != null) nav.swipeBackBlockers++ else if (nav.swipeBackBlockers > 0) nav.swipeBackBlockers-- }
    }
    val blur by animateDpAsState(if (menu != null) 14.dp else 0.dp, tween(LancatMotion.DUR_SLOW), label = "blur")

    var showAttach by rememberSaveable { mutableStateOf(debug.attachOpen) }
    var forwardIds by remember { mutableStateOf<List<String>?>(null) }
    val kbState = rememberKeyboardPanelState()
    LaunchedEffect(debug.emojiOpen) { if (debug.emojiOpen) kbState.debugOpenEmoji() }
    LaunchedEffect(items.size > 0) {
        if (items.isEmpty()) return@LaunchedEffect
        debug.selectIndicesFromEnd.forEach { i -> (items.filterIsInstance<ChatItem.Msg>().getOrNull(i))?.let { selection[it.m.id] = true } }
    }

    val targets: () -> List<String> = {
        if (isAll) peers.filter { it.online && !it.blocked }.map { it.id } else listOf(peerId)
    }
    val viewing = remember { mutableStateOf<Body.File?>(null) }
    val fileActions = remember(repo) {
        object : FileActions {
            override fun pause(id: String) { scope.launch { repo.pause(id) } }
            override fun resume(id: String) { scope.launch { repo.resume(id) } }
            override fun retry(id: String) { scope.launch { repo.retry(id) } }
            override fun cancel(id: String) { scope.launch { repo.cancel(id) } }
            override fun open(body: Body.File) {
                // 图片在应用内全屏看；其他文件交给系统关联的应用
                if (showsAsImage(body)) viewing.value = body
                else if (!FileIntents.open(ctx, body)) flash.show(if (body.localUri == null) "文件不存在" else "没有可以打开此文件的应用")
            }
        }
    }
    var lastTypingSent by remember { mutableLongStateOf(0L) }
    val coordsMap = remember(peerId) { HashMap<String, LayoutCoordinates>() }

    Box(Modifier.fillMaxSize().background(c.background)) {
        Column(
            Modifier.fillMaxSize().then(
                if (blur > 0.dp && Build.VERSION.SDK_INT >= 31) Modifier.blur(blur) else Modifier
            )
        ) {
            if (selecting) {
                SelectionBar(
                    count = selection.size,
                    onClose = { selection.clear() },
                    onForward = { forwardIds = selection.keys.toList() },
                    onDelete = {
                        val ids = selection.keys.toList(); selection.clear()
                        scope.launch { ids.forEach { repo.deleteMessage(it) }; flash.show("已删除 ${ids.size} 条") }
                    },
                )
            } else {
                ChatHeader(peer, isAll, peers.count { it.online && !it.blocked }, typingUntil > 0, onBack)
            }
            Box(Modifier.weight(1f).fillMaxWidth()) {
                val now = LocalClock.current.now()
                if (messages.isEmpty()) {
                    EmptyChat(peer, isAll)
                }
                // 反向布局：index 0 = 最新，贴着底部，键盘 / 表情面板顶起时最新消息跟着上移、不跳动；
                // verticalArrangement = Top：消息不满一屏时从顶部（标题栏下方）往下排，而不是沉在底部（桌面 / Telegram 一致），
                // 满一屏后与普通反向列表完全一样（新消息在底部、打开即在最新处）
                LazyColumn(
                    state = listState,
                    reverseLayout = true,
                    verticalArrangement = Arrangement.Top,
                    modifier = Modifier.fillMaxSize().testTag("chat-list"),
                    contentPadding = PaddingValues(start = 8.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
                ) {
                    items(items, key = { it.key }, contentType = ::contentTypeOf) { item ->
                        when (item) {
                            is ChatItem.Day -> DayChip(Fmt.dateLabel(item.dayStart, now), Modifier.animateItem())
                            is ChatItem.Msg -> {
                                val isNew = item.m.id !in seen && item.m.timeMs >= openedNewest
                                MessageRow(
                                    item = item,
                                    isNew = isNew,
                                    onEnterDone = { seen.add(item.m.id) },
                                    selecting = selecting,
                                    selected = selection[item.m.id] == true,
                                    senderName = if (item.showSender) peerNames[item.m.peerId] ?: item.m.peerId else null,
                                    actions = fileActions,
                                    play = menu == null,
                                    onCoords = { coordsMap[item.m.id] = it },
                                    onClick = {
                                        if (selecting) {
                                            if (selection.remove(item.m.id) == null) selection[item.m.id] = true
                                        } else {
                                            // 点已完成的文件 / 图片直接打开（Telegram）
                                            val b = item.m.body
                                            if (b is Body.File && b.state is TransferState.Done && b.localUri != null) fileActions.open(b)
                                        }
                                    },
                                    onLongClick = {
                                        if (selecting) { selection[item.m.id] = true }
                                        else {
                                            val co = coordsMap[item.m.id]
                                            val b = if (co != null && co.isAttached) co.boundsInRoot() else Rect.Zero
                                            menu = MenuTarget(item, b)
                                        }
                                    },
                                    modifier = Modifier.animateItem(),
                                )
                            }
                        }
                    }
                    if (loadingOlder) item(key = "loading", contentType = "loading") {
                        Box(Modifier.fillMaxWidth().padding(12.dp), contentAlignment = Alignment.Center) {
                            androidx.compose.material3.CircularProgressIndicator(Modifier.size(20.dp), color = c.text3, strokeWidth = 2.dp)
                        }
                    }
                }
                FloatingDate(listState, items, now)
                ScrollToBottom(listState) { scope.launch { listState.animateScrollToItem(0) } }
            }
            val canSend = isAll || (peer != null && !peer.blocked)
            ComposerArea(
                state = kbState,
                placeholder = when {
                    isAll -> "发给全部在线设备…"
                    peer?.blocked == true -> "已拉黑 ${peer.name}"
                    else -> "发给 ${peer?.name ?: "对方"}…"
                },
                enabled = canSend,
                onSendText = { t -> scope.launch { repo.sendText(targets(), t) } },
                onTyping = {
                    val t = System.currentTimeMillis()
                    if (!isAll && t - lastTypingSent > 3000) { lastTypingSent = t; scope.launch { repo.typing(peerId) } }
                },
                onAttach = { showAttach = true },
                onSticker = { e -> scope.launch { repo.sendSticker(targets(), e.code) } },
            )
        }

        // 长按菜单浮层
        AnimatedVisibility(menu != null, enter = fadeIn(tween(LancatMotion.DUR)), exit = fadeOut(tween(LancatMotion.DUR))) {
            val target = remember { mutableStateOf<MenuTarget?>(null) }
            if (menu != null) target.value = menu
            target.value?.let { t ->
                MessageMenuOverlay(
                    t, fileActions,
                    onDismiss = { menu = null },
                    onCopy = { (t.item.m.body as? Body.Text)?.let { copyToClipboard(ctx, it.text); flash.show("已复制") }; menu = null },
                    onDelete = { scope.launch { repo.deleteMessage(t.item.m.id) }; menu = null },
                    onRetry = { scope.launch { repo.retry(t.item.m.id) }; menu = null },
                    onReveal = {
                        (t.item.m.body as? Body.File)?.let { f -> if (!FileIntents.reveal(ctx, f)) flash.show("没有可用的文件管理器") }
                        menu = null
                    },
                    onForward = { forwardIds = listOf(t.item.m.id); menu = null },
                    onSelect = { selection[t.item.m.id] = true; menu = null },
                )
            }
        }
        if (menu == null && debug.menuOnMessageIndexFromEnd != null) {
            LaunchedEffect(Unit) {
                delay(50)
                val it = items.filterIsInstance<ChatItem.Msg>().getOrNull(debug.menuOnMessageIndexFromEnd) ?: return@LaunchedEffect
                val co = coordsMap[it.m.id]
                menu = MenuTarget(it, if (co != null && co.isAttached) co.boundsInRoot() else Rect(0f, 600f, 600f, 700f))
            }
        }
        // 全屏看图（盖在整个聊天页上）
        viewing.value?.let { f ->
            ImageViewer(
                f,
                onDismiss = { viewing.value = null },
                onOpenWith = { if (!FileIntents.open(ctx, f)) flash.show("没有可以打开此文件的应用") },
                onReveal = { if (!FileIntents.reveal(ctx, f)) flash.show("没有可用的文件管理器") },
            )
        }
    }

    if (showAttach) {
        AttachSheet(
            onDismiss = { showAttach = false },
            onUris = { uris -> showAttach = false; if (uris.isNotEmpty()) scope.launch { repo.sendFiles(targets(), uris) } },
            onClipboardText = { t -> showAttach = false; scope.launch { repo.sendText(targets(), t) } },
        )
    }
    forwardIds?.let { ids ->
        PeerPickerSheet(
            repo = repo,
            title = "转发 ${ids.size} 条到…",
            exclude = if (isAll) null else peerId,
            onDismiss = { forwardIds = null },
            onPick = { picked ->
                forwardIds = null
                selection.clear()
                val msgs = messages.filter { it.id in ids }
                scope.launch {
                    for (m in msgs) when (val b = m.body) {
                        is Body.Text -> repo.sendText(picked, b.text)
                        is Body.Sticker -> repo.sendSticker(picked, b.emojiId)
                        is Body.File -> b.localUri?.let { repo.sendFiles(picked, listOf(it)) }
                    }
                    flash.show("已转发到 ${picked.size} 台设备")
                }
            },
        )
    }
}

// ------------------------------------------------------------------------------------------- 顶栏

@Composable
private fun ChatHeader(peer: Peer?, isAll: Boolean, onlineCount: Int, typing: Boolean, onBack: () -> Unit) {
    val c = LancatTheme.colors
    LcTopBar(
        navigation = { LcIconButton(LancatIcons.Back, "返回", onBack, tint = c.accent, size = 44.dp, iconSize = 24.dp) },
        title = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (isAll) AllAvatar(40.dp)
                else if (peer != null) PeerAvatar(peer.id, peer.name, 40.dp, online = peer.online, blocked = peer.blocked)
                Spacer(Modifier.width(10.dp))
                Column {
                    Text(
                        if (isAll) "全部设备" else peer?.name ?: "未知设备",
                        style = LancatTheme.type.name, color = c.text, maxLines = 1, overflow = TextOverflow.Ellipsis,
                    )
                    val sub: String
                    val subColor: Color
                    when {
                        typing -> { sub = "正在输入…"; subColor = c.accent }
                        isAll -> { sub = "$onlineCount 台在线"; subColor = c.text2 }
                        peer == null -> { sub = "离线"; subColor = c.text3 }
                        else -> {
                            sub = buildList {
                                add(if (peer.online) "在线" else "离线")
                                if (peer.os.isNotEmpty()) add(Fmt.os(peer.os))
                                if (peer.relayVia != null) add("经 ${peer.relayVia} 中转") else if (peer.ip.isNotEmpty()) add(peer.ip)
                            }.joinToString(" · ")
                            subColor = if (peer.online) c.text2 else c.text3
                        }
                    }
                    if (typing) TypingText(sub, subColor) else
                        Text(sub, style = LancatTheme.type.caption, color = subColor, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
        },
    )
}

@Composable
private fun TypingText(text: String, color: Color) {
    val t = androidx.compose.animation.core.rememberInfiniteTransition(label = "typing")
    val a by t.animateFloat(0.45f, 1f, androidx.compose.animation.core.infiniteRepeatable(tween(600), androidx.compose.animation.core.RepeatMode.Reverse), label = "a")
    Text(text, style = LancatTheme.type.caption, color = color, modifier = Modifier.graphicsLayer { alpha = a })
}

@Composable
private fun SelectionBar(count: Int, onClose: () -> Unit, onForward: () -> Unit, onDelete: () -> Unit) {
    val c = LancatTheme.colors
    LcTopBar(
        navigation = { LcIconButton(LancatIcons.X, "取消多选", onClose, tint = c.text, size = 44.dp) },
        title = { Text("已选择 $count 条", style = LancatTheme.type.title, color = c.text) },
        actions = {
            LcIconButton(LancatIcons.get("forward"), "转发", onForward, tint = c.accent)
            LcIconButton(LancatIcons.get("trash"), "删除", onDelete, tint = c.danger)
        },
    )
}

// ------------------------------------------------------------------------------------------- 消息行

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun MessageRow(
    item: ChatItem.Msg,
    isNew: Boolean,
    onEnterDone: () -> Unit,
    selecting: Boolean,
    selected: Boolean,
    senderName: String?,
    actions: FileActions,
    play: Boolean,
    onCoords: (LayoutCoordinates) -> Unit,
    onClick: () -> Unit,
    onLongClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val c = LancatTheme.colors
    val m = item.m
    val out = m.dir == Direction.OUT
    // 入场：桌面 msg-in，translateY(8px) scale(.97) → 0，320ms 轻回弹
    val enter = remember { Animatable(if (isNew) 0f else 1f) }
    LaunchedEffect(Unit) {
        if (isNew) { enter.animateTo(1f, tween(320, easing = LancatMotion.MsgIn)); onEnterDone() }
    }
    val density = LocalDensity.current
    val selBg by animateFloatAsState(if (selected) 1f else 0f, tween(LancatMotion.DUR_FAST), label = "sel")
    Row(
        modifier
            .fillMaxWidth()
            .graphicsLayer {
                val p = enter.value
                alpha = p.coerceIn(0f, 1f)
                translationY = (1f - p) * with(density) { 10.dp.toPx() }
                val s = 0.97f + 0.03f * p
                scaleX = s; scaleY = s
                transformOrigin = androidx.compose.ui.graphics.TransformOrigin(if (out) 1f else 0f, 1f)
            }
            .background(c.accent.copy(alpha = 0.10f * selBg))
            .combinedClickable(
                interactionSource = remember { MutableInteractionSource() },
                indication = null,
                onClick = onClick,
                onLongClick = onLongClick,
            )
            .padding(top = if (item.joinTop) 2.dp else 8.dp),
        verticalAlignment = Alignment.Bottom,
    ) {
        AnimatedVisibility(selecting, enter = fadeIn() + scaleIn(), exit = fadeOut() + scaleOut()) {
            SelectCircle(selected, Modifier.padding(start = 4.dp, end = 10.dp, bottom = 6.dp))
        }
        Column(
            Modifier.weight(1f),
            horizontalAlignment = if (out) Alignment.End else Alignment.Start,
        ) {
            if (senderName != null) {
                Text(
                    if (out) "→ $senderName" else senderName,
                    style = LancatTheme.type.caption, fontWeight = FontWeight.Medium,
                    color = if (out) c.text3 else AvatarColors.base(m.peerId),
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 2.dp),
                )
            }
            Box(Modifier.onGloballyPositioned(onCoords)) {
                MessageContent(item, actions, play)
            }
        }
    }
}

@Composable
private fun MessageContent(item: ChatItem.Msg, actions: FileActions, play: Boolean, modifier: Modifier = Modifier) {
    val m = item.m
    when (val b = m.body) {
        is Body.Text -> {
            val solo = remember(b.text) { soloEmojiCode(b.text) }
            if (solo != null) StickerMessage(m, solo, play, modifier)
            else TextBubble(m, b.text, item.joinTop, item.joinBottom, modifier)
        }
        is Body.Sticker -> StickerMessage(m, b.emojiId, play, modifier)
        is Body.File -> if (showsAsImage(b)) ImageMessage(m, b, actions, item.joinTop, item.joinBottom, modifier)
            else FileCard(m, b, actions, item.joinTop, item.joinBottom, modifier)
    }
}

/** 同桌面 soloEmoji：整条消息只有一个目录里的表情时按贴纸显示。 */
private fun soloEmojiCode(text: String): String? {
    val t = text.trim()
    if (t.isEmpty() || t.length > 8) return null
    return fan.lancat.ui.stub.EmojiCatalog.all.firstOrNull { it.ch == t }?.code
}

@Composable
private fun DayChip(label: String, modifier: Modifier = Modifier) {
    Box(modifier.fillMaxWidth().padding(vertical = 8.dp), contentAlignment = Alignment.Center) {
        DatePill(label)
    }
}

@Composable
private fun DatePill(label: String, modifier: Modifier = Modifier) {
    val c = LancatTheme.colors
    Text(
        label, style = LancatTheme.type.caption, fontWeight = FontWeight.Medium,
        color = if (c.isDark) c.text2 else Color.White,
        modifier = modifier
            .background(if (c.isDark) c.bubbleIn else Color(0x66707579), RoundedCornerShape(12.dp))
            .padding(horizontal = 10.dp, vertical = 3.dp),
    )
}

/** 滚动时悬浮在顶部的日期标签（Telegram）：停止滚动 1.2 秒后淡出。 */
@Composable
private fun FloatingDate(state: androidx.compose.foundation.lazy.LazyListState, items: List<ChatItem>, now: Long) {
    val label by remember(items) {
        derivedStateOf {
            val top = state.layoutInfo.visibleItemsInfo.lastOrNull() ?: return@derivedStateOf null
            when (val it = items.getOrNull(top.index)) {
                is ChatItem.Msg -> Fmt.dateLabel(it.m.timeMs, now)
                is ChatItem.Day -> Fmt.dateLabel(it.dayStart, now)
                else -> null
            }
        }
    }
    var visible by remember { mutableStateOf(false) }
    LaunchedEffect(state) {
        snapshotFlow { state.isScrollInProgress }.collect { s ->
            if (s) visible = true else { delay(1200); visible = false }
        }
    }
    Box(Modifier.fillMaxWidth().padding(top = 8.dp), contentAlignment = Alignment.TopCenter) {
        AnimatedVisibility(visible && label != null, enter = fadeIn(tween(LancatMotion.DUR)), exit = fadeOut(tween(LancatMotion.DUR_SLOW))) {
            DatePill(label ?: "")
        }
    }
}

@Composable
private fun ScrollToBottom(state: androidx.compose.foundation.lazy.LazyListState, onClick: () -> Unit) {
    val c = LancatTheme.colors
    val show by remember { derivedStateOf { state.firstVisibleItemIndex > 3 } }
    Box(Modifier.fillMaxSize().padding(12.dp), contentAlignment = Alignment.BottomEnd) {
        AnimatedVisibility(show, enter = fadeIn() + scaleIn(initialScale = 0.6f), exit = fadeOut() + scaleOut(targetScale = 0.6f)) {
            Box(
                Modifier.size(44.dp).shadow(4.dp, CircleShape).background(c.card, CircleShape).clickable(onClick = onClick),
                contentAlignment = Alignment.Center,
            ) {
                Icon(LancatIcons.get("down"), "回到底部", tint = c.text2, modifier = Modifier.size(24.dp))
            }
        }
    }
}

@Composable
private fun EmptyChat(peer: Peer?, isAll: Boolean) {
    // 同桌面空会话：直接写在聊天背景上的两行淡色居中文字，不套卡片 / 气泡容器
    val c = LancatTheme.colors
    Column(
        Modifier.fillMaxSize().padding(horizontal = 32.dp).testTag("chat-empty"),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("还没有消息", style = LancatTheme.type.bodySmall, fontWeight = FontWeight.SemiBold, color = c.text2, textAlign = TextAlign.Center)
        Spacer(Modifier.height(4.dp))
        Text(
            if (isAll) "这里会汇总所有设备的消息" else "发一句话、一个表情或任意文件给 ${peer?.name ?: "对方"}",
            style = LancatTheme.type.caption, color = c.text3, textAlign = TextAlign.Center,
        )
    }
}

// ------------------------------------------------------------------------------------------- 长按菜单

@Composable
private fun MessageMenuOverlay(
    t: MenuTarget,
    actions: FileActions,
    onDismiss: () -> Unit,
    onCopy: () -> Unit,
    onDelete: () -> Unit,
    onRetry: () -> Unit,
    onReveal: () -> Unit,
    onForward: () -> Unit,
    onSelect: () -> Unit,
) {
    val c = LancatTheme.colors
    val m = t.item.m
    val out = m.dir == Direction.OUT
    val density = LocalDensity.current
    val pop = remember { Animatable(0.92f) }
    LaunchedEffect(Unit) { pop.animateTo(1f, tween(LancatMotion.DUR_SLOW, easing = LancatMotion.EaseSpring)) }
    var rootH by remember { mutableStateOf(0f) }
    var rootW by remember { mutableStateOf(0f) }
    var rootTop by remember { mutableStateOf(0f) }
    Box(
        Modifier
            .fillMaxSize()
            .onGloballyPositioned { rootH = it.size.height.toFloat(); rootW = it.size.width.toFloat(); rootTop = it.boundsInRoot().top }
            .background(c.scrim)
            .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null, onClick = onDismiss)
    ) {
        val b = t.bounds
        val items = buildList<Triple<String, ImageVector, () -> Unit>> {
            val body = m.body
            if (body is Body.Text) add(Triple("复制", LancatIcons.get("copy"), onCopy))
            add(Triple("转发", LancatIcons.get("forward"), onForward))
            if (body is Body.File) {
                val s = body.state
                if (s is TransferState.Failed && s.retryable || (s is TransferState.Cancelled && out)) add(Triple("重试", LancatIcons.get("retry"), onRetry))
                if (s is TransferState.Done) add(Triple("在文件夹中显示", LancatIcons.get("reveal"), onReveal))
            }
            add(Triple("多选", LancatIcons.get("selectall"), onSelect))
            add(Triple("删除", LancatIcons.get("trash"), onDelete))
        }
        val menuW = with(density) { 220.dp.toPx() }
        val menuH = with(density) { (items.size * 48 + 12).dp.toPx() }
        val margin = with(density) { 10.dp.toPx() }
        val localTop = b.top - rootTop
        val localBottom = b.bottom - rootTop
        // 放得下就放在气泡下方，否则上方；再不行就把气泡往上挪（Telegram）
        val below = localBottom + margin + menuH <= rootH - margin
        val shiftUp = if (!below && localTop - margin - menuH < margin) (localBottom + margin + menuH) - (rootH - margin) else 0f
        val bubbleY = localTop - shiftUp.coerceAtLeast(0f)
        val menuY = if (below || shiftUp > 0f) bubbleY + b.height + margin else localTop - margin - menuH
        val menuX = if (out) (b.right - menuW).coerceAtLeast(margin) else b.left.coerceIn(margin, (rootW - menuW - margin).coerceAtLeast(margin)) // 首帧 rootW 还是 0

        // 气泡副本
        Box(
            Modifier
                .offset { IntOffset(b.left.roundToInt(), bubbleY.roundToInt()) }
                .graphicsLayer {
                    scaleX = 1f + 0.03f * (pop.value - 0.92f) / 0.08f; scaleY = scaleX
                    transformOrigin = androidx.compose.ui.graphics.TransformOrigin(if (out) 1f else 0f, 0.5f)
                }
        ) {
            MessageContent(t.item, actions, play = true)
        }
        // 菜单
        Column(
            Modifier
                .offset { IntOffset(menuX.roundToInt(), menuY.roundToInt()) }
                .graphicsLayer {
                    scaleX = pop.value; scaleY = pop.value; alpha = ((pop.value - 0.92f) / 0.08f).coerceIn(0f, 1f)
                    transformOrigin = androidx.compose.ui.graphics.TransformOrigin(if (out) 1f else 0f, if (menuY > bubbleY) 0f else 1f)
                }
                .width(220.dp)
                .shadow(16.dp, LancatShapes.card)
                .background(c.card, LancatShapes.card)
                .padding(vertical = 6.dp)
        ) {
            items.forEachIndexed { i, (label, icon, cb) ->
                val danger = label == "删除"
                Row(
                    Modifier.fillMaxWidth().height(48.dp).clickable(onClick = cb).padding(horizontal = 16.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(icon, null, tint = if (danger) c.danger else c.text2, modifier = Modifier.size(20.dp))
                    Spacer(Modifier.width(14.dp))
                    Text(label, style = LancatTheme.type.body, color = if (danger) c.danger else c.text)
                }
            }
        }
    }
}

