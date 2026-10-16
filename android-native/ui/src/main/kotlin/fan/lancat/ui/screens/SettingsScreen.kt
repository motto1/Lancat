package fan.lancat.ui.screens

import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsBottomHeight
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fan.lancat.model.LancatRepository
import fan.lancat.model.Settings
import fan.lancat.model.UpdateState
import fan.lancat.ui.update.LocalUpdater
import fan.lancat.ui.update.updateStatusText
import fan.lancat.ui.components.LancatIcons
import fan.lancat.ui.components.LcIconButton
import fan.lancat.ui.components.LcTopBar
import fan.lancat.ui.components.LocalFlash
import fan.lancat.ui.components.PeerAvatar
import fan.lancat.ui.components.TitleText
import fan.lancat.ui.components.copyToClipboard
import fan.lancat.ui.stub.EmojiCatalog
import fan.lancat.ui.theme.LancatMotion
import fan.lancat.ui.theme.LancatShapes
import fan.lancat.ui.theme.LancatTheme
import fan.lancat.ui.util.Fmt
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * 设置页：对齐桌面 Settings.svelte（本机 / 接收 / 设备与黑名单 / 通用 / 数据 / 关于），
 * 字段以契约 Settings 为准；桌面独有的「停靠」「开机自启」「单文件上限」在 Android 上没有对应项。
 */
@Composable
fun SettingsScreen(repo: LancatRepository, onBack: () -> Unit) {
    val c = LancatTheme.colors
    val s by repo.settings.collectAsStateWithLifecycle()
    val self by repo.self.collectAsStateWithLifecycle()
    val peers by repo.peers.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val ctx = LocalContext.current
    val flash = LocalFlash.current
    fun update(f: (Settings) -> Settings) { scope.launch { repo.updateSettings(f) } }

    val pickDir = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri: Uri? ->
        if (uri != null) {
            runCatching {
                ctx.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            }
            update { it.copy(receiveDir = uri.toString()) }
        }
    }

    Column(Modifier.fillMaxSize().background(c.background)) {
        LcTopBar(
            navigation = { LcIconButton(LancatIcons.Back, "返回", onBack, tint = c.accent, size = 44.dp, iconSize = 24.dp) },
            title = { TitleText("设置") },
        )
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
            // ---- 本机
            Section("本机") {
                NameRow(s.deviceName, self?.name ?: "") { v -> update { it.copy(deviceName = v) } }
                Sep()
                InfoRow("设备 ID", self?.id ?: "—", mono = true) {
                    self?.id?.let { copyToClipboard(ctx, it); flash.show("已复制设备 ID") }
                }
                Sep()
                InfoRow("地址", self?.let { "${it.ip} · TCP/UDP ${it.port}" } ?: "—", mono = true)
            }
            // ---- 接收
            Section("接收", note = "收到的文件直接保存到接收目录、自动重名。常驻接收会显示一条常驻通知，关闭后切到后台一段时间将收不到文件。") {
                Row(Modifier.fillMaxWidth().heightIn(min = 52.dp).padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("接收目录", style = LancatTheme.type.body, color = c.text)
                        Text(
                            dirLabel(s.receiveDir), style = LancatTheme.type.caption, color = c.text2,
                            maxLines = 1, overflow = TextOverflow.MiddleEllipsis,
                        )
                    }
                    if (s.receiveDir.isNotEmpty()) {
                        SmallButton("恢复默认") { update { it.copy(receiveDir = "") } }
                        Spacer(Modifier.width(6.dp))
                    }
                    SmallButton("选择…") { pickDir.launch(null) }
                }
                Sep()
                SwitchRow("常驻接收", "后台保持可被发现、随时接收", s.keepAlive) { v -> update { it.copy(keepAlive = v) } }
                Sep()
                SwitchRow("收到后自动打开", "仅对单个文件生效", s.autoOpenReceived) { v -> update { it.copy(autoOpenReceived = v) } }
            }
            // ---- 通知
            Section("通知") {
                SwitchRow("消息通知", "收到消息时通知，可直接回复", s.notifyMessages) { v -> update { it.copy(notifyMessages = v) } }
            }
            // ---- 通用
            Section("通用") {
                Row(Modifier.fillMaxWidth().height(56.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text("外观", style = LancatTheme.type.body, color = c.text, modifier = Modifier.weight(1f))
                    Segmented(
                        listOf("system" to "跟随系统", "light" to "浅色", "dark" to "深色"),
                        s.theme,
                    ) { v -> update { it.copy(theme = v) } }
                }
            }
            // ---- 设备与黑名单
            Section("设备与黑名单", note = if (peers.isEmpty()) "还没有发现其他设备。" else null) {
                peers.forEachIndexed { i, p ->
                    if (i > 0) Sep(start = 52.dp)
                    Row(Modifier.fillMaxWidth().height(60.dp), verticalAlignment = Alignment.CenterVertically) {
                        PeerAvatar(p.id, p.name, 38.dp, online = p.online, dimmed = !p.online, blocked = p.blocked)
                        Spacer(Modifier.width(14.dp))
                        Column(Modifier.weight(1f)) {
                            Text(p.name, style = LancatTheme.type.body, color = c.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            Text(
                                listOf(Fmt.os(p.os).ifEmpty { "未知系统" }, p.ip.ifEmpty { "—" }).joinToString(" · "),
                                style = LancatTheme.type.caption, color = c.text2,
                            )
                        }
                        SmallButton(if (p.blocked) "取消拉黑" else "拉黑", danger = !p.blocked) {
                            scope.launch { repo.setBlocked(p.id, !p.blocked) }
                        }
                    }
                }
                if (peers.isNotEmpty()) Sep()
                Column(Modifier.padding(vertical = 12.dp)) {
                    Text("手动添加设备", style = LancatTheme.type.caption, color = c.text2, modifier = Modifier.padding(bottom = 8.dp))
                    ManualAddField(repo, Modifier.fillMaxWidth())
                }
            }
            // ---- 数据
            Section("数据") {
                var confirm by remember { mutableStateOf(false) }
                LaunchedEffect(confirm) { if (confirm) { delay(3000); confirm = false } }
                Row(Modifier.fillMaxWidth().height(56.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text("聊天记录只保存在本机", style = LancatTheme.type.body, color = c.text, modifier = Modifier.weight(1f))
                    SmallButton(if (confirm) "再点一次确认" else "清空全部记录", danger = true) {
                        if (!confirm) confirm = true
                        else { confirm = false; scope.launch { repo.clearHistory(null); flash.show("已清空全部聊天记录") } }
                    }
                }
            }
            // ---- 关于
            Section("关于") {
                val updater = LocalUpdater.current
                val ver = updater?.currentVersion
                    ?: remember { runCatching { ctx.packageManager.getPackageInfo(ctx.packageName, 0).versionName }.getOrNull() ?: "dev" }
                InfoRow("当前版本", ver)
                Sep()
                if (updater != null) {
                    val us by updater.state.collectAsStateWithLifecycle()
                    val auto by updater.autoCheck.collectAsStateWithLifecycle()
                    Row(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("检查更新", style = LancatTheme.type.body, color = c.text)
                            Text(
                                updateStatusText(us, ver), style = LancatTheme.type.caption,
                                color = if (us is UpdateState.Failed) c.danger else if (us is UpdateState.Available || us is UpdateState.ReadyToInstall) c.accent else c.text2,
                                maxLines = 2, overflow = TextOverflow.Ellipsis,
                            )
                        }
                        Spacer(Modifier.width(8.dp))
                        when (us) {
                            is UpdateState.Available, is UpdateState.Downloading, is UpdateState.Verifying, is UpdateState.ReadyToInstall ->
                                SmallButton("查看") { updater.check(manual = true) }
                            UpdateState.Checking -> SmallButton("检查中…") {}
                            else -> SmallButton("检查更新") { updater.check(manual = true) }
                        }
                    }
                    Sep()
                    SwitchRow("自动检查更新", "启动时和每 6 小时从 GitHub 检查一次", auto) { updater.setAutoCheck(it) }
                    Sep()
                }
                Column(Modifier.padding(vertical = 14.dp)) {
                    Text("Lancat $ver · Android · 局域网零配置快传", style = LancatTheme.type.body, color = c.text)
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "内置 ${EmojiCatalog.all.size} 个动态表情来自 Google Noto Animated Emoji，以 CC BY 4.0 许可使用（https://creativecommons.org/licenses/by/4.0/）。",
                        style = LancatTheme.type.caption, color = c.text2,
                    )
                }
            }
            Spacer(Modifier.height(24.dp))
            Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars))
        }
    }
}

private fun dirLabel(v: String): String {
    if (v.isEmpty()) return "下载/Lancat（默认）"
    // content://com.android.externalstorage.documents/tree/primary%3ADocuments%2FLancat → 内部存储/Documents/Lancat
    val dec = Uri.decode(v.substringAfter("/tree/", v))
    return dec.replace("primary:", "内部存储/")
}

@Composable
private fun Section(title: String, note: String? = null, content: @Composable ColumnScope.() -> Unit) {
    val c = LancatTheme.colors
    Text(
        title, style = LancatTheme.type.section, color = c.text2,
        modifier = Modifier.padding(start = 14.dp, top = 22.dp, bottom = 7.dp),
    )
    Column(
        Modifier.fillMaxWidth().background(c.card, LancatShapes.card).padding(horizontal = 14.dp),
        content = content,
    )
    if (note != null) {
        Text(note, style = LancatTheme.type.caption, color = c.text3, modifier = Modifier.padding(start = 14.dp, end = 14.dp, top = 7.dp))
    }
}

@Composable
private fun Sep(start: androidx.compose.ui.unit.Dp = 0.dp) {
    Box(Modifier.fillMaxWidth().padding(start = start).height(0.5.dp).background(LancatTheme.colors.sep))
}

@Composable
private fun NameRow(value: String, fallback: String, onChange: (String) -> Unit) {
    val c = LancatTheme.colors
    var text by rememberSaveable(value) { mutableStateOf(value) }
    LaunchedEffect(text) {
        if (text != value) { delay(400); onChange(text.trim()) }
    }
    Row(Modifier.fillMaxWidth().height(52.dp), verticalAlignment = Alignment.CenterVertically) {
        Text("设备名称", style = LancatTheme.type.body, color = c.text, modifier = Modifier.width(96.dp))
        Box(Modifier.weight(1f), contentAlignment = Alignment.CenterEnd) {
            if (text.isEmpty()) Text(fallback, style = LancatTheme.type.body, color = c.text3, textAlign = TextAlign.End)
            BasicTextField(
                text, { text = it }, singleLine = true,
                textStyle = LancatTheme.type.body.copy(color = c.text2, textAlign = TextAlign.End),
                cursorBrush = SolidColor(c.accent),
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

@Composable
private fun InfoRow(label: String, value: String, mono: Boolean = false, onClick: (() -> Unit)? = null) {
    val c = LancatTheme.colors
    Row(
        Modifier.fillMaxWidth().height(48.dp).then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, style = LancatTheme.type.body, color = c.text, modifier = Modifier.width(96.dp))
        Text(
            value, style = LancatTheme.type.bodySmall.copy(fontFamily = if (mono) FontFamily.Monospace else FontFamily.Default),
            color = c.text2, textAlign = TextAlign.End, maxLines = 1, overflow = TextOverflow.MiddleEllipsis,
            modifier = Modifier.weight(1f),
        )
    }
}

@Composable
private fun SwitchRow(title: String, sub: String?, checked: Boolean, onChange: (Boolean) -> Unit) {
    val c = LancatTheme.colors
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp)
            .clickable(remember { MutableInteractionSource() }, null) { onChange(!checked) }
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = LancatTheme.type.body, color = c.text)
            if (sub != null) Text(sub, style = LancatTheme.type.caption, color = c.text2)
        }
        LcSwitch(checked, onChange)
    }
}

/** iOS / 桌面 .switch 风格开关：51×31，白色圆钮带投影。 */
@Composable
fun LcSwitch(checked: Boolean, onChange: (Boolean) -> Unit) {
    val c = LancatTheme.colors
    val track by animateColorAsState(if (checked) c.online else (if (c.isDark) Color(0x52787880) else Color(0x29787880)), tween(LancatMotion.DUR_SLOW), label = "track")
    val x by animateDpAsState(if (checked) 22.dp else 2.dp, tween(LancatMotion.DUR_SLOW, easing = LancatMotion.EaseSpring), label = "knob")
    Box(
        Modifier.size(51.dp, 31.dp).background(track, CircleShape)
            .clickable(remember { MutableInteractionSource() }, null) { onChange(!checked) }
    ) {
        Box(
            Modifier.offset(x = x, y = 2.dp).size(27.dp).shadow(2.dp, CircleShape).background(Color.White, CircleShape)
        )
    }
}

@Composable
private fun SmallButton(text: String, danger: Boolean = false, onClick: () -> Unit) {
    val c = LancatTheme.colors
    Text(
        text, style = LancatTheme.type.bodySmall, fontWeight = FontWeight.Medium,
        color = if (danger) c.danger else c.accent,
        modifier = Modifier
            .background(if (danger) c.danger.copy(alpha = 0.10f) else c.accent.copy(alpha = 0.10f), RoundedCornerShape(15.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 6.dp),
    )
}

/** 分段控件（桌面 .seg）。 */
@Composable
private fun Segmented(options: List<Pair<String, String>>, value: String, onChange: (String) -> Unit) {
    val c = LancatTheme.colors
    val idx = options.indexOfFirst { it.first == value }.coerceAtLeast(0)
    val segW = 72.dp
    val x by animateDpAsState(segW * idx, tween(LancatMotion.DUR_SLOW, easing = LancatMotion.Ease), label = "seg")
    Box(
        Modifier.size(segW * options.size + 4.dp, 34.dp)
            .background(if (c.isDark) Color(0x1FFFFFFF) else Color(0x14000000), RoundedCornerShape(9.dp))
            .padding(2.dp)
    ) {
        Box(
            Modifier.offset(x = x).size(segW, 30.dp)
                .shadow(if (c.isDark) 0.dp else 1.5.dp, RoundedCornerShape(7.dp))
                .background(if (c.isDark) Color(0x2BFFFFFF) else Color.White, RoundedCornerShape(7.dp))
        )
        Row(Modifier.fillMaxHeight()) {
            options.forEachIndexed { i, (k, label) ->
                Box(
                    Modifier.width(segW).fillMaxHeight()
                        .clickable(remember { MutableInteractionSource() }, null) { onChange(k) },
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        label, style = LancatTheme.type.caption,
                        fontWeight = if (i == idx) FontWeight.SemiBold else FontWeight.Normal,
                        color = if (i == idx) c.text else c.text2,
                    )
                }
            }
        }
    }
}

