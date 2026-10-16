package fan.lancat.ui.screens

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsBottomHeight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.DpOffset
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fan.lancat.model.LancatRepository
import fan.lancat.model.Peer
import fan.lancat.ui.components.AllAvatar
import fan.lancat.ui.components.ClipContent
import fan.lancat.ui.components.LancatIcons
import fan.lancat.ui.components.LcIconButton
import fan.lancat.ui.components.LcTopBar
import fan.lancat.ui.components.LocalFlash
import fan.lancat.ui.components.PeerAvatar
import fan.lancat.ui.components.TitleText
import fan.lancat.ui.components.UnreadBadge
import fan.lancat.ui.components.readClipboard
import fan.lancat.ui.theme.LancatShapes
import fan.lancat.ui.theme.LancatTheme
import fan.lancat.ui.util.Fmt
import fan.lancat.ui.util.LocalClock
import kotlinx.coroutines.launch

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PeerListScreen(
    repo: LancatRepository,
    onOpen: (String) -> Unit,
    onSettings: () -> Unit,
) {
    val c = LancatTheme.colors
    val peers by repo.peers.collectAsStateWithLifecycle()
    val self by repo.self.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var refreshing by remember { mutableStateOf(false) }
    var showAdd by rememberSaveable { mutableStateOf(false) }
    val onlineCount = peers.count { it.online && !it.blocked }

    Column(Modifier.fillMaxSize().background(c.list)) {
        LcTopBar(
            title = {
                TitleText(
                    "Lancat",
                    sub = when {
                        refreshing -> "正在刷新…"
                        self == null -> "正在启动…"
                        else -> "本机 · ${self!!.name} · ${self!!.ip}"
                    },
                )
            },
            actions = {
                LcIconButton(LancatIcons.Plus, "手动添加设备", { showAdd = true })
                LcIconButton(LancatIcons.Gear, "设置", onSettings)
            },
        )
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = {
                scope.launch {
                    refreshing = true
                    try { repo.refreshPeers() } finally { refreshing = false }
                }
            },
            modifier = Modifier.fillMaxSize(),
        ) {
            if (peers.isEmpty()) {
                EmptyRadar(repo, self?.name)
            } else {
                LazyColumn(Modifier.fillMaxSize()) {
                    item(key = "*", contentType = "all") {
                        AllRow(onlineCount, peers.sumOf { it.unread }) { onOpen("*") }
                    }
                    items(peers, key = { it.id }, contentType = { "peer" }) { p ->
                        PeerRow(p.toRowModel(), repo, onClick = { onOpen(p.id) }, modifier = Modifier.animateItem())
                    }
                    item(key = "footer", contentType = "footer") {
                        Text(
                            "${peers.size} 台设备 · $onlineCount 台在线",
                            style = LancatTheme.type.caption, color = c.text3, textAlign = TextAlign.Center,
                            modifier = Modifier.fillMaxWidth().padding(vertical = 18.dp),
                        )
                        Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars))
                    }
                }
            }
        }
    }
    if (showAdd) ManualAddDialog(repo, onDismiss = { showAdd = false })
}

/** 行的稳定快照：只有这些字段变化才重组该行。 */
@Immutable
private data class PeerRowModel(
    val id: String, val name: String, val os: String, val online: Boolean, val relayVia: String?,
    val unread: Int, val preview: String?, val previewOut: Boolean, val timeMs: Long?, val blocked: Boolean,
)

private fun Peer.toRowModel() = PeerRowModel(
    id, name, os, online, relayVia, unread, lastMessage?.text, lastMessage?.outgoing == true, lastMessage?.timeMs, blocked,
)

@Composable
private fun AllRow(online: Int, unread: Int, onClick: () -> Unit) {
    val c = LancatTheme.colors
    RowShell(onClick = onClick, onLongClick = null) {
        AllAvatar(54.dp)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text("全部设备", style = LancatTheme.type.name, color = c.text)
            Spacer(Modifier.height(2.dp))
            Text(
                if (online > 0) "发给所有在线设备 · $online 台在线" else "暂无在线设备",
                style = LancatTheme.type.bodySmall, color = c.text2, maxLines = 1,
            )
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RowShell(onClick: () -> Unit, onLongClick: (() -> Unit)?, content: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .height(76.dp)
            .combinedClickable(onClick = onClick, onLongClick = onLongClick)
            .padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        content = content,
    )
}

@Composable
private fun PeerRow(p: PeerRowModel, repo: LancatRepository, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val c = LancatTheme.colors
    val now = LocalClock.current.now()
    var menu by remember { mutableStateOf(false) }
    val ctx = LocalContext.current
    val flash = LocalFlash.current
    val scope = rememberCoroutineScope()
    Box(modifier) {
        RowShell(onClick = onClick, onLongClick = { menu = true }) {
            PeerAvatar(p.id, p.name, 54.dp, online = p.online, dimmed = !p.online, blocked = p.blocked)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                  Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        p.name, style = LancatTheme.type.name, color = c.text, maxLines = 1,
                        overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false),
                    )
                    val meta = buildList {
                        if (p.os.isNotEmpty()) add(Fmt.os(p.os))
                        if (p.relayVia != null) add("经 ${p.relayVia} 中转")
                        if (p.blocked) add("已拉黑")
                    }.joinToString(" · ")
                    if (meta.isNotEmpty()) {
                        Spacer(Modifier.width(6.dp))
                        Text(
                            meta, style = LancatTheme.type.caption,
                            color = if (p.relayVia != null && !p.blocked) c.accent.copy(alpha = 0.85f) else c.text3,
                            maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false),
                        )
                    }
                  }
                    if (p.timeMs != null) {
                        Spacer(Modifier.width(8.dp))
                        Text(
                            Fmt.listTime(p.timeMs, now), style = LancatTheme.type.caption,
                            color = if (p.unread > 0) c.accent else c.text3,
                        )
                    }
                }
                Spacer(Modifier.height(3.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    val text = when {
                        p.preview == null -> if (p.online) "在线 · 点按开始传输" else "离线"
                        p.previewOut -> "你：${p.preview}"
                        else -> p.preview
                    }
                    Text(
                        text.replace('\n', ' '), style = LancatTheme.type.bodySmall,
                        color = if (p.preview == null) c.text3 else c.text2,
                        maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
                    )
                    if (p.unread > 0) {
                        Spacer(Modifier.width(8.dp))
                        UnreadBadge(p.unread, muted = p.blocked)
                    }
                }
            }
        }
        // 分隔线从文字起始处开始（Telegram）
        Box(Modifier.align(Alignment.BottomStart).padding(start = 78.dp).fillMaxWidth().height(0.5.dp).background(c.sep))
        DropdownMenu(
            expanded = menu,
            onDismissRequest = { menu = false },
            offset = DpOffset(78.dp, (-8).dp),
            shape = LancatShapes.card,
            containerColor = c.card,
        ) {
            MenuItem(if (p.blocked) "取消拉黑" else "拉黑", LancatIcons.get(if (p.blocked) "unblock" else "block"), danger = !p.blocked) {
                menu = false
                scope.launch { repo.setBlocked(p.id, !p.blocked); flash.show(if (p.blocked) "已取消拉黑" else "已拉黑 ${p.name}") }
            }
            MenuItem("清空记录", LancatIcons.get("trash")) {
                menu = false
                scope.launch { repo.clearHistory(p.id); flash.show("已清空与 ${p.name} 的记录") }
            }
            MenuItem("发送剪贴板", LancatIcons.get("clipboard"), enabled = p.online && !p.blocked) {
                menu = false
                when (val clip = readClipboard(ctx)) {
                    null -> flash.show("剪贴板是空的")
                    is ClipContent.Text -> scope.launch { repo.sendText(listOf(p.id), clip.text); flash.show("已发送剪贴板文字") }
                    is ClipContent.Uris -> scope.launch { repo.sendFiles(listOf(p.id), clip.uris); flash.show("已发送 ${clip.uris.size} 个文件") }
                }
            }
        }
    }
}

@Composable
internal fun MenuItem(text: String, icon: ImageVector, danger: Boolean = false, enabled: Boolean = true, onClick: () -> Unit) {
    val c = LancatTheme.colors
    val col = if (danger) c.danger else c.text
    DropdownMenuItem(
        text = { Text(text, style = LancatTheme.type.body, color = if (enabled) col else c.text3) },
        leadingIcon = { Icon(icon, null, tint = if (enabled) (if (danger) c.danger else c.text2) else c.text3, modifier = Modifier.size(20.dp)) },
        onClick = onClick,
        enabled = enabled,
    )
}

/** 空态：桌面 Empty.svelte 的三圈雷达 + 手动添加。 */
@Composable
private fun EmptyRadar(repo: LancatRepository, selfName: String?) {
    val c = LancatTheme.colors
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Spacer(Modifier.height(96.dp))
        Radar(Modifier.size(132.dp))
        Spacer(Modifier.height(20.dp))
        Text("正在寻找局域网内的设备…", style = LancatTheme.type.name, color = c.text)
        Spacer(Modifier.height(8.dp))
        Text(
            "在其他电脑或手机上打开 Lancat，它们会自动出现在这里。" + (selfName?.let { "本机名为「$it」。" } ?: ""),
            style = LancatTheme.type.bodySmall, color = c.text2, textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(24.dp))
        ManualAddField(repo, Modifier.fillMaxWidth())
        Spacer(Modifier.height(10.dp))
        Text("同一 Wi-Fi 下下拉可重新扫描", style = LancatTheme.type.caption, color = c.text3)
    }
}

@Composable
fun Radar(modifier: Modifier = Modifier) {
    val c = LancatTheme.colors
    val t = rememberInfiniteTransition(label = "radar")
    val phase by t.animateFloat(0f, 1f, infiniteRepeatable(tween(2400, easing = LinearEasing), RepeatMode.Restart), label = "p")
    Box(modifier, contentAlignment = Alignment.Center) {
        for (i in 0 until 3) {
            Box(
                Modifier.fillMaxSize().graphicsLayer {
                    val p = (phase + i / 3f) % 1f
                    // cubic-bezier(0.2,0.6,0.4,1) 近似：先快后慢
                    val e = 1f - (1f - p) * (1f - p)
                    scaleX = 0.35f + 0.65f * e; scaleY = scaleX
                    alpha = (1f - p) * 0.9f
                }.border(1.5.dp, c.accent, CircleShape)
            )
        }
        Box(Modifier.size(52.dp).background(c.accent.copy(alpha = 0.12f), CircleShape), contentAlignment = Alignment.Center) {
            Icon(LancatIcons.get("radar"), null, tint = c.accent, modifier = Modifier.size(28.dp))
        }
    }
}

@Composable
fun ManualAddField(repo: LancatRepository, modifier: Modifier = Modifier, onAdded: () -> Unit = {}) {
    val c = LancatTheme.colors
    var addr by rememberSaveable { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val flash = LocalFlash.current
    fun submit() {
        val a = addr.trim()
        if (a.isEmpty() || busy) return
        busy = true
        scope.launch {
            try { repo.addManualPeer(a); addr = ""; flash.show("已添加 $a"); onAdded() }
            catch (e: Exception) { flash.show("添加失败：${e.message ?: e}") }
            finally { busy = false }
        }
    }
    Row(
        modifier.height(46.dp).background(c.field, RoundedCornerShape(23.dp)).border(0.5.dp, c.sep, RoundedCornerShape(23.dp))
            .padding(start = 18.dp, end = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.weight(1f), contentAlignment = Alignment.CenterStart) {
            if (addr.isEmpty()) Text("手动添加：192.168.1.20", style = LancatTheme.type.body, color = c.text3)
            BasicTextField(
                addr, { addr = it }, singleLine = true,
                textStyle = LancatTheme.type.body.copy(color = c.text),
                cursorBrush = SolidColor(c.accent),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { submit() }),
                modifier = Modifier.fillMaxWidth(),
            )
        }
        Box(
            Modifier.size(38.dp).background(if (addr.isBlank()) c.accent.copy(alpha = 0.35f) else c.accent, CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            LcIconButton(LancatIcons.Plus, "添加", { submit() }, tint = Color.White, size = 38.dp, iconSize = 20.dp, enabled = !busy)
        }
    }
}

@Composable
private fun ManualAddDialog(repo: LancatRepository, onDismiss: () -> Unit) {
    val c = LancatTheme.colors
    androidx.compose.material3.AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = c.card,
        shape = RoundedCornerShape(20.dp),
        title = { Text("手动添加设备", style = LancatTheme.type.title, color = c.text) },
        text = {
            Column {
                Text("输入对方的 IP（可带端口），对方不在同一网段或组播被路由器屏蔽时使用。", style = LancatTheme.type.bodySmall, color = c.text2)
                Spacer(Modifier.height(14.dp))
                ManualAddField(repo, Modifier.fillMaxWidth(), onAdded = onDismiss)
            }
        },
        confirmButton = {
            androidx.compose.material3.TextButton(onClick = onDismiss) { Text("完成", color = c.accent, fontWeight = FontWeight.Medium) }
        },
    )
}
