package fan.lancat.ui.screens.chat

import android.app.DownloadManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import fan.lancat.model.Body
import fan.lancat.model.Direction
import fan.lancat.model.Message
import fan.lancat.model.TransferState
import fan.lancat.ui.components.LancatIcons
import fan.lancat.ui.components.LcIconButton
import fan.lancat.sticker.StickerImage
import fan.lancat.ui.theme.LancatShapes
import fan.lancat.ui.theme.LancatTheme
import fan.lancat.ui.util.Fmt
import kotlin.math.ceil
import kotlin.math.max

/** 气泡形状：18dp 圆角，组内相邻一侧收成 6dp，组尾（最下一条）靠外下角是 6dp 尾角。 */
fun bubbleShape(out: Boolean, joinTop: Boolean, joinBottom: Boolean): RoundedCornerShape {
    val r = LancatShapes.bubbleRadius
    val t = LancatShapes.bubbleTail
    return if (out) {
        RoundedCornerShape(topStart = r, topEnd = if (joinTop) t else r, bottomEnd = t, bottomStart = r)
    } else {
        RoundedCornerShape(topStart = if (joinTop) t else r, topEnd = r, bottomEnd = r, bottomStart = t)
    }.let { if (joinBottom) it else it }
}

/** 文本气泡：Telegram 式时间/勾号嵌在最后一行右侧，放不下才换行。 */
@Composable
fun TextBubble(m: Message, text: String, joinTop: Boolean, joinBottom: Boolean, modifier: Modifier = Modifier) {
    val c = LancatTheme.colors
    val out = m.dir == Direction.OUT
    val fg = if (out) c.accentContrast else c.text
    val metaColor = if (out) Color.White.copy(alpha = 0.72f) else c.text3
    Box(
        modifier
            .widthIn(max = 300.dp)
            .background(if (out) c.bubbleOut else c.bubbleIn, bubbleShape(out, joinTop, joinBottom))
            .padding(start = 12.dp, end = 10.dp, top = 7.dp, bottom = 6.dp)
    ) {
        TextWithMeta(
            text = text,
            color = fg,
            meta = { MetaLabel(m, metaColor) },
        )
    }
}

@Composable
fun MetaLabel(m: Message, color: Color, modifier: Modifier = Modifier) {
    Row(modifier.padding(top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(Fmt.hm(m.timeMs), fontSize = 11.sp, lineHeight = 13.sp, color = color)
        if (m.dir == Direction.OUT) {
            Spacer(Modifier.width(3.dp))
            Icon(
                LancatIcons.get(if (m.delivered) "checks" else "check"), if (m.delivered) "已送达" else "已发送",
                tint = color, modifier = Modifier.size(14.dp),
            )
        }
    }
}

@Composable
private fun TextWithMeta(text: String, color: Color, meta: @Composable () -> Unit) {
    val holder = remember { arrayOfNulls<TextLayoutResult>(1) }
    val style = LancatTheme.type.body
    Layout(content = {
        Text(text, style = style, color = color, onTextLayout = { holder[0] = it })
        meta()
    }) { ms, cons ->
        val gap = 8.dp.roundToPx()
        val tp = ms[0].measure(cons.copy(minWidth = 0, minHeight = 0))
        val mp = ms[1].measure(Constraints())
        val l = holder[0]
        val lastW = if (l != null && l.lineCount > 0) {
            val i = l.lineCount - 1
            ceil(l.getLineRight(i) - l.getLineLeft(i)).toInt()
        } else tp.width
        val inline = lastW + gap + mp.width <= cons.maxWidth
        val w: Int; val h: Int
        if (inline) {
            w = max(tp.width, lastW + gap + mp.width)
            h = max(tp.height, mp.height)
        } else {
            w = max(tp.width, mp.width)
            h = tp.height + mp.height
        }
        layout(w, h) {
            tp.place(0, 0)
            mp.place(w - mp.width, h - mp.height + if (inline) 2.dp.roundToPx() else 0)
        }
    }
}

@Composable
fun StickerMessage(m: Message, emojiId: String, play: Boolean, modifier: Modifier = Modifier) {
    val c = LancatTheme.colors
    Box(modifier) {
        StickerImage(emojiId = emojiId, size = 150.dp, play = play)
        Box(
            Modifier.align(Alignment.BottomEnd).padding(4.dp)
                .background(c.datePill.copy(alpha = 0.45f), RoundedCornerShape(10.dp))
                .padding(horizontal = 6.dp, vertical = 1.dp)
        ) { MetaLabel(m, Color.White, Modifier.padding(top = 0.dp)) }
    }
}

/** 文件卡片动作。 */
interface FileActions {
    fun pause(id: String); fun resume(id: String); fun retry(id: String); fun cancel(id: String)
    fun open(body: Body.File)
}

fun fileIconName(name: String): String {
    val ext = name.substringAfterLast('.', "").lowercase()
    return when (ext) {
        "png", "jpg", "jpeg", "gif", "webp", "heic", "bmp", "svg", "avif" -> "image"
        "mp4", "mov", "mkv", "avi", "webm" -> "film"
        "mp3", "flac", "wav", "m4a", "ogg", "aac" -> "music"
        "zip", "7z", "rar", "tar", "gz", "xz", "dmg", "iso", "apk" -> "archive"
        else -> "doc"
    }
}

/** 文件卡片：图标、名称、大小·进度·速度·剩余时间、进度条、动作按钮（同桌面 MessageItem.svelte）。 */
@Composable
fun FileCard(m: Message, f: Body.File, actions: FileActions, joinTop: Boolean, joinBottom: Boolean, modifier: Modifier = Modifier) {
    val c = LancatTheme.colors
    val out = m.dir == Direction.OUT
    val s = f.state
    val failed = s is TransferState.Failed
    val (done, total) = when (s) {
        is TransferState.Running -> s.bytes to s.total
        is TransferState.Paused -> s.bytes to s.total
        is TransferState.Done -> f.size to f.size
        else -> 0L to f.size
    }
    val pct = if (total > 0) (done.toFloat() / total).coerceIn(0f, 1f) else 0f
    val showBar = s is TransferState.Running || s is TransferState.Paused || s is TransferState.Queued
    val bg = if (out) c.accent.copy(alpha = if (c.isDark) 0.22f else 0.12f) else c.bubbleIn
    val shape = bubbleShape(out, joinTop, joinBottom)
    Row(
        modifier
            .width(286.dp)
            .graphicsLayer { alpha = if (s is TransferState.Cancelled) 0.6f else 1f }
            .clip(shape)
            .background(bg)
            .border(0.5.dp, if (failed) c.danger.copy(alpha = 0.5f) else c.sep, shape)
            .padding(start = 10.dp, end = 4.dp, top = 10.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // 图标：完成时点按打开，传输中显示环形进度
        Box(Modifier.size(46.dp), contentAlignment = Alignment.Center) {
            Box(
                Modifier.size(46.dp).background(if (s is TransferState.Done) c.accent else c.field, RoundedCornerShape(12.dp)),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    LancatIcons.get(fileIconName(f.name)), null,
                    tint = if (s is TransferState.Done) Color.White else c.accent, modifier = Modifier.size(24.dp),
                )
            }
        }
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(f.name, style = LancatTheme.type.bodySmall, fontWeight = FontWeight.Medium, color = c.text, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
            Spacer(Modifier.height(2.dp))
            val metaText = fileMeta(m, f, pct)
            Text(metaText, style = LancatTheme.type.caption, color = if (failed) c.danger else c.text2, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (showBar) {
                Spacer(Modifier.height(6.dp))
                val anim by animateFloatAsState(pct, tween(200), label = "pct")
                val barColor = if (s is TransferState.Paused || s is TransferState.Queued) c.text3 else c.accent
                Box(
                    Modifier.fillMaxWidth().height(4.dp).drawBehind {
                        val r = CornerRadius(size.height / 2)
                        drawRoundRect(c.sep, cornerRadius = r)
                        drawRoundRect(barColor, size = Size(size.width * anim, size.height), cornerRadius = r)
                    }
                )
            }
            Row(Modifier.fillMaxWidth().padding(top = 3.dp), horizontalArrangement = androidx.compose.foundation.layout.Arrangement.End) {
                MetaLabel(m, c.text3, Modifier.padding(top = 0.dp))
            }
        }
        Spacer(Modifier.width(2.dp))
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            FileButtons(m, f, actions)
        }
    }
}

@Composable
private fun FileButtons(m: Message, f: Body.File, a: FileActions) {
    val c = LancatTheme.colors
    val out = m.dir == Direction.OUT
    val btn: @Composable (String, String, () -> Unit, Color) -> Unit = { icon, desc, cb, tint ->
        LcIconButton(LancatIcons.get(icon), desc, cb, tint = tint, size = 36.dp, iconSize = 18.dp)
    }
    when (val s = f.state) {
        is TransferState.Running -> {
            btn("pause", "暂停", { a.pause(m.id) }, c.text2)
            btn("x", "取消", { a.cancel(m.id) }, c.text2)
        }
        is TransferState.Paused -> {
            btn("play", "继续", { a.resume(m.id) }, c.accent)
            btn("x", "取消", { a.cancel(m.id) }, c.text2)
        }
        is TransferState.Queued -> btn("x", "取消", { a.cancel(m.id) }, c.text2)
        is TransferState.Failed -> {
            if (s.retryable) btn("retry", "重试", { a.retry(m.id) }, c.accent)
            if (out || s.retryable) btn("x", "取消", { a.cancel(m.id) }, c.text2)
        }
        is TransferState.Done -> if (f.localUri != null) btn("reveal", "打开", { a.open(f) }, c.accent)
        is TransferState.Cancelled -> if (out) btn("retry", "重新发送", { a.retry(m.id) }, c.text2)
    }
}

fun fileMeta(m: Message, f: Body.File, pct: Float): String {
    val out = m.dir == Direction.OUT
    val size = Fmt.size(f.size)
    return when (val s = f.state) {
        is TransferState.Running -> {
            val p = "${(pct * 100).toInt()}%"
            if (s.bytesPerSec >= 1) listOf("$p", "${Fmt.size(s.bytesPerSec)}/s", Fmt.eta(s.etaSec)).filter { it.isNotEmpty() }.joinToString(" · ")
                .let { "${Fmt.size(s.bytes)} / $size · $it" }
            else "$size · $p · ${if (out) "发送中" else "接收中"}"
        }
        is TransferState.Paused -> "${Fmt.size(s.bytes)} / $size · ${(pct * 100).toInt()}% · 已暂停"
        TransferState.Queued -> "$size · 排队中"
        TransferState.Done -> "$size · ${if (out) "已送达" else "已保存"}"
        is TransferState.Failed -> "$size · 失败：${s.reason}"
        TransferState.Cancelled -> "$size · 已取消"
    }
}

/** 打开收到 / 发出的文件。 */
fun openFile(ctx: Context, f: Body.File): Boolean {
    val uri = f.localUri ?: return false
    val i = Intent(Intent.ACTION_VIEW).apply {
        setDataAndType(Uri.parse(uri), f.mime ?: "*/*")
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
    }
    return try { ctx.startActivity(Intent.createChooser(i, "打开 ${f.name}").addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)); true }
    catch (e: ActivityNotFoundException) { false } catch (e: SecurityException) { false }
}

/** 「在文件夹中显示」：默认接收目录是 Download/Lancat，打开系统下载内容视图。 */
fun revealInFolder(ctx: Context): Boolean = try {
    ctx.startActivity(Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)); true
} catch (e: ActivityNotFoundException) { false }

@Composable
fun SelectCircle(selected: Boolean, modifier: Modifier = Modifier, size: Dp = 22.dp) {
    val c = LancatTheme.colors
    Box(
        modifier.size(size)
            .background(if (selected) c.accent else Color.Transparent, CircleShape)
            .border(1.5.dp, if (selected) c.accent else c.text3, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        if (selected) Icon(LancatIcons.get("check"), null, tint = Color.White, modifier = Modifier.size(size * 0.7f))
    }
}

