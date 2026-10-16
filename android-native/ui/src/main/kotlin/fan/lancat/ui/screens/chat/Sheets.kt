package fan.lancat.ui.screens.chat

import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsBottomHeight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fan.lancat.model.LancatRepository
import fan.lancat.model.Peer
import fan.lancat.ui.components.ClipContent
import fan.lancat.ui.components.LancatIcons
import fan.lancat.ui.components.LocalFlash
import fan.lancat.ui.components.PeerAvatar
import fan.lancat.ui.components.readClipboard
import fan.lancat.ui.theme.LancatTheme
import fan.lancat.ui.util.Fmt

/**
 * 附件抽屉：相册（Photo Picker，免权限）、文件（SAF OpenMultipleDocuments）、剪贴板。
 * 只负责拿到 content:// uri 交给调用方（调用方再 repo.sendFiles）。
 * launcher 注册在抽屉外层，抽屉关闭 / Activity 重建都不会丢结果。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AttachSheet(
    onDismiss: () -> Unit,
    onUris: (List<String>) -> Unit,
    onClipboardText: (String) -> Unit,
) {
    val c = LancatTheme.colors
    val ctx = LocalContext.current
    val flash = LocalFlash.current
    val media = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(maxItems = 50)) { uris ->
        if (uris.isEmpty()) onDismiss() else onUris(uris.map(Uri::toString))
    }
    val docs = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        // 断点续传可能在很久之后，尽量拿长期读权限
        uris.forEach { runCatching { ctx.contentResolver.takePersistableUriPermission(it, Intent.FLAG_GRANT_READ_URI_PERMISSION) } }
        if (uris.isEmpty()) onDismiss() else onUris(uris.map(Uri::toString))
    }
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheet,
        containerColor = c.surface,
        shape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp),
        dragHandle = {
            Box(Modifier.padding(top = 8.dp, bottom = 4.dp).size(36.dp, 4.dp).background(c.text3, CircleShape))
        },
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp)) {
            Text("发送", style = LancatTheme.type.title, color = c.text, modifier = Modifier.padding(vertical = 8.dp))
            Row(Modifier.fillMaxWidth().padding(vertical = 12.dp), horizontalArrangement = Arrangement.SpaceEvenly) {
                AttachTile("相册", LancatIcons.get("image"), Color(0xFF0A84FF)) {
                    media.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
                }
                AttachTile("文件", LancatIcons.get("doc"), Color(0xFFFF9F0A)) { docs.launch(arrayOf("*/*")) }
                AttachTile("剪贴板", LancatIcons.get("clipboard"), Color(0xFF30D158)) {
                    when (val clip = readClipboard(ctx)) {
                        null -> flash.show("剪贴板是空的")
                        is ClipContent.Text -> onClipboardText(clip.text)
                        is ClipContent.Uris -> onUris(clip.uris)
                    }
                }
            }
            Text(
                "文件直接点对点传输，不经过任何服务器；断开后可断点续传。",
                style = LancatTheme.type.caption, color = c.text3,
                modifier = Modifier.padding(top = 4.dp, bottom = 16.dp),
            )
            Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars))
        }
    }
}

@Composable
private fun AttachTile(label: String, icon: ImageVector, color: Color, onClick: () -> Unit) {
    val c = LancatTheme.colors
    Column(
        Modifier.width(84.dp).clickable(onClick = onClick).padding(vertical = 6.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Box(Modifier.size(58.dp).background(color, CircleShape), contentAlignment = Alignment.Center) {
            Icon(icon, null, tint = Color.White, modifier = Modifier.size(28.dp))
        }
        Spacer(Modifier.height(8.dp))
        Text(label, style = LancatTheme.type.bodySmall, color = c.text2)
    }
}

/** 选目标设备的抽屉（转发用）。 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PeerPickerSheet(
    repo: LancatRepository,
    title: String,
    exclude: String?,
    onDismiss: () -> Unit,
    onPick: (List<String>) -> Unit,
) {
    val c = LancatTheme.colors
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = c.surface,
        shape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp),
        dragHandle = { Box(Modifier.padding(top = 8.dp, bottom = 4.dp).size(36.dp, 4.dp).background(c.text3, CircleShape)) },
    ) {
        PeerPickList(repo, title, "发送", exclude, onPick)
        Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars))
    }
}

/** 设备多选列表 + 底部发送按钮；转发抽屉和系统分享入口共用。 */
@Composable
fun PeerPickList(
    repo: LancatRepository,
    title: String,
    action: String,
    exclude: String?,
    onPick: (List<String>) -> Unit,
    modifier: Modifier = Modifier,
    preselect: List<String> = emptyList(),
) {
    val c = LancatTheme.colors
    val peers by repo.peers.collectAsStateWithLifecycle()
    val list = remember(peers, exclude) { peers.filter { it.id != exclude && !it.blocked } }
    val picked = remember { mutableStateMapOf<String, Boolean>().apply { preselect.forEach { put(it, true) } } }
    Column(modifier.fillMaxWidth()) {
        Text(title, style = LancatTheme.type.title, color = c.text, modifier = Modifier.padding(horizontal = 20.dp, vertical = 10.dp))
        if (list.isEmpty()) {
            Text("没有可选的设备", style = LancatTheme.type.bodySmall, color = c.text3, modifier = Modifier.padding(20.dp))
        }
        LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp).weight(1f, fill = false)) {
            items(list, key = { it.id }, contentType = { "peer" }) { p ->
                PickRow(p, picked[p.id] == true) { if (picked.remove(p.id) == null) picked[p.id] = true }
            }
        }
        val n = picked.size
        Box(
            Modifier
                .padding(horizontal = 16.dp, vertical = 12.dp)
                .fillMaxWidth()
                .height(48.dp)
                .background(if (n > 0) c.accent else c.accent.copy(alpha = 0.35f), RoundedCornerShape(24.dp))
                .clickable(enabled = n > 0) { onPick(list.filter { picked[it.id] == true }.map { it.id }) },
            contentAlignment = Alignment.Center,
        ) {
            Text(if (n > 0) "$action（$n）" else action, color = Color.White, style = LancatTheme.type.body, fontWeight = FontWeight.SemiBold)
        }
    }
}

@Composable
private fun PickRow(p: Peer, on: Boolean, onToggle: () -> Unit) {
    val c = LancatTheme.colors
    Row(
        Modifier.fillMaxWidth().height(64.dp).clickable(onClick = onToggle).padding(horizontal = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        PeerAvatar(p.id, p.name, 44.dp, online = p.online, dimmed = !p.online)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(p.name, style = LancatTheme.type.name, color = c.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(
                listOfNotNull(if (p.online) "在线" else "离线（上线后发送）", Fmt.os(p.os).ifEmpty { null }).joinToString(" · "),
                style = LancatTheme.type.caption, color = if (p.online) c.text2 else c.text3,
            )
        }
        SelectCircle(on)
    }
}
