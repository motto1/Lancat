package fan.lancat.ui.update

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fan.lancat.model.AppRelease
import fan.lancat.model.AppUpdater
import fan.lancat.model.UpdateState
import fan.lancat.ui.theme.LancatShapes
import fan.lancat.ui.theme.LancatTheme
import fan.lancat.ui.util.Fmt

/** 应用内更新（:app 的 Updater）；截图 / 预览里没有时为 null，设置页就只显示版本号。 */
val LocalUpdater = staticCompositionLocalOf<AppUpdater?> { null }

private fun releaseOf(s: UpdateState): AppRelease? = when (s) {
    is UpdateState.Available -> s.release
    is UpdateState.Downloading -> s.release
    is UpdateState.Verifying -> s.release
    is UpdateState.ReadyToInstall -> s.release
    is UpdateState.Failed -> s.release
    else -> null
}

/** 设置页「检查更新」右侧的一行状态文字 */
fun updateStatusText(s: UpdateState, current: String): String = when (s) {
    UpdateState.Idle -> "从 GitHub 发布页获取新版本"
    UpdateState.Checking -> "正在检查…"
    is UpdateState.UpToDate -> "已是最新版本"
    is UpdateState.Available -> "发现新版本 ${s.release.version}"
    is UpdateState.Downloading -> if (s.total > 0) "正在下载 ${(s.bytes * 100 / s.total).coerceIn(0, 100)}%" else "正在下载 ${Fmt.size(s.bytes)}"
    is UpdateState.Verifying -> "正在校验…"
    is UpdateState.ReadyToInstall -> if (s.needsPermission) "需要允许安装未知应用" else "已下载 ${s.release.version}，可安装"
    is UpdateState.Failed -> s.message
}

/** 新版本对话框：更新说明、下载进度、校验 / 安装授权。放在 LancatApp 根部。 */
@Composable
fun UpdateDialogHost(updater: AppUpdater) {
    val visible by updater.dialogVisible.collectAsStateWithLifecycle()
    val s by updater.state.collectAsStateWithLifecycle()
    val rel = releaseOf(s) ?: return
    if (!visible) return
    Dialog(onDismissRequest = { updater.hideDialog() }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        UpdateCard(s, rel, updater.currentVersion, updater)
    }
}

@Composable
fun UpdateCard(s: UpdateState, rel: AppRelease, current: String, updater: AppUpdater?) {
    val c = LancatTheme.colors
    Column(
        Modifier.padding(24.dp).widthIn(max = 420.dp).fillMaxWidth()
            .background(c.card, LancatShapes.card).padding(20.dp).testTag("update-dialog"),
    ) {
        Text(
            when (s) {
                is UpdateState.Failed -> "更新失败"
                is UpdateState.ReadyToInstall -> "新版本已下载"
                else -> "发现新版本"
            },
            style = LancatTheme.type.title, color = c.text,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            "Lancat ${rel.version}（当前 $current）" + if (rel.apkSize > 0) " · ${Fmt.size(rel.apkSize)}" else "",
            style = LancatTheme.type.caption, color = c.text2,
        )
        if (rel.notes.isNotBlank() && s !is UpdateState.Failed) {
            Spacer(Modifier.height(12.dp))
            Box(
                Modifier.fillMaxWidth().heightIn(max = 220.dp)
                    .background(if (c.isDark) Color(0x14FFFFFF) else Color(0x0A000000), RoundedCornerShape(10.dp))
                    .verticalScroll(rememberScrollState()).padding(12.dp)
            ) {
                Text(cleanNotes(rel.notes), style = LancatTheme.type.bodySmall, color = c.text)
            }
        }
        Spacer(Modifier.height(14.dp))
        when (s) {
            is UpdateState.Downloading -> {
                val p = if (s.total > 0) (s.bytes.toFloat() / s.total).coerceIn(0f, 1f) else null
                if (p != null) LinearProgressIndicator(progress = { p }, Modifier.fillMaxWidth(), color = c.accent, trackColor = c.sep)
                else LinearProgressIndicator(Modifier.fillMaxWidth(), color = c.accent, trackColor = c.sep)
                Spacer(Modifier.height(6.dp))
                Text(
                    if (s.total > 0) "${Fmt.size(s.bytes)} / ${Fmt.size(s.total)}" else Fmt.size(s.bytes),
                    style = LancatTheme.type.caption, color = c.text2,
                )
            }
            is UpdateState.Verifying -> Text("正在校验 SHA-256…", style = LancatTheme.type.caption, color = c.text2)
            is UpdateState.ReadyToInstall -> Text(
                if (s.needsPermission) "SHA-256 校验通过。安装前需要在系统设置里允许 Lancat「安装未知应用」，授权后返回即可继续。"
                else "SHA-256 校验通过，点「安装」交给系统完成更新，聊天记录和设置都会保留。",
                style = LancatTheme.type.caption, color = c.text2,
            )
            is UpdateState.Failed -> Text(s.message, style = LancatTheme.type.bodySmall, color = c.danger)
            else -> Text("下载后会校验 SHA-256 再安装，聊天记录和设置都会保留。", style = LancatTheme.type.caption, color = c.text2)
        }
        Spacer(Modifier.height(16.dp))
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End, verticalAlignment = Alignment.CenterVertically) {
            when (s) {
                is UpdateState.Available -> {
                    DialogButton("跳过此版本", secondary = true) { updater?.skip() }
                    Spacer(Modifier.weight(1f))
                    DialogButton("稍后", secondary = true) { updater?.hideDialog() }
                    DialogButton("更新") { updater?.download() }
                }
                is UpdateState.Downloading, is UpdateState.Verifying -> DialogButton("后台下载", secondary = true) { updater?.hideDialog() }
                is UpdateState.ReadyToInstall -> {
                    DialogButton("稍后", secondary = true) { updater?.hideDialog() }
                    DialogButton(if (s.needsPermission) "去允许" else "安装") { updater?.install() }
                }
                is UpdateState.Failed -> {
                    DialogButton("关闭", secondary = true) { updater?.hideDialog() }
                    DialogButton("重试") { updater?.download() }
                }
                else -> DialogButton("关闭", secondary = true) { updater?.hideDialog() }
            }
        }
    }
}

/** GitHub 自动生成的说明里的 Markdown 标记去掉，保留文字 */
internal fun cleanNotes(md: String): String = md.lines().joinToString("\n") { l ->
    l.trimEnd().replace(Regex("""^#{1,6}\s*"""), "").replace(Regex("""^\s*[*-]\s+"""), "• ").replace("**", "").replace("`", "")
}.replace(Regex("""\n{3,}"""), "\n\n").trim()

@Composable
private fun DialogButton(text: String, secondary: Boolean = false, onClick: () -> Unit) {
    val c = LancatTheme.colors
    Text(
        text, style = LancatTheme.type.bodySmall, fontWeight = FontWeight.SemiBold,
        color = if (secondary) c.text2 else c.accentContrast,
        modifier = Modifier.padding(start = 8.dp)
            .background(if (secondary) Color.Transparent else c.accent, RoundedCornerShape(17.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
    )
}
