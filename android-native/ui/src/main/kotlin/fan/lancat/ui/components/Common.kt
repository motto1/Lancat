package fan.lancat.ui.components

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.ripple
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import fan.lancat.ui.theme.AvatarColors
import fan.lancat.ui.theme.LancatMotion
import fan.lancat.ui.theme.LancatTheme

/** 设备头像：hsl 渐变圆 + 两个字母，与桌面 Sidebar.svelte .avatar 相同。 */
@Composable
fun PeerAvatar(
    id: String,
    name: String,
    size: Dp,
    modifier: Modifier = Modifier,
    online: Boolean = false,
    showDot: Boolean = true,
    dimmed: Boolean = false,
    blocked: Boolean = false,
) {
    val c = LancatTheme.colors
    val top = remember(id) { AvatarColors.top(id) }
    val base = remember(id) { AvatarColors.base(id) }
    val text = remember(name) { AvatarColors.initials(name) }
    Box(modifier.size(size)) {
        Box(
            Modifier
                .size(size)
                .graphicsLayer { alpha = if (dimmed) 0.5f else 1f }
                .background(
                    if (blocked) Brush.verticalGradient(listOf(Color(0xFFB8B8BD), Color(0xFF8E8E93)))
                    else Brush.verticalGradient(listOf(top, base)),
                    CircleShape,
                ),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text,
                color = Color.White,
                fontSize = (size.value * 0.36f).sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
            )
        }
        if (online && showDot) {
            val dot = (size * 0.27f).coerceAtLeast(11.dp)
            Box(
                Modifier
                    .align(Alignment.BottomEnd)
                    .size(dot)
                    .background(c.list, CircleShape)
                    .padding(dot * 0.17f)
                    .background(c.online, CircleShape)
            )
        }
    }
}

/** 「全部」入口的头像：灰底网格图标（桌面 .all-avatar）。 */
@Composable
fun AllAvatar(size: Dp, modifier: Modifier = Modifier, active: Boolean = false) {
    val c = LancatTheme.colors
    Box(
        modifier.size(size).background(if (active) c.accent else c.bubbleIn, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Icon(LancatIcons.Grid, null, tint = if (active) c.accentContrast else c.text2, modifier = Modifier.size(size * 0.42f))
    }
}

@Composable
fun UnreadBadge(count: Int, modifier: Modifier = Modifier, muted: Boolean = false) {
    val c = LancatTheme.colors
    Box(
        modifier
            .defaultMinSize(minWidth = 22.dp)
            .height(22.dp)
            .background(if (muted) c.text3 else c.accent, CircleShape)
            .padding(horizontal = 6.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            if (count > 99) "99+" else count.toString(),
            color = Color.White,
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold,
            textAlign = TextAlign.Center,
        )
    }
}

/** 带按压缩放的图标按钮（桌面 .icon-btn：按下 0.94，弹簧回弹）。 */
@Composable
fun LcIconButton(
    icon: ImageVector,
    contentDescription: String?,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    tint: Color = LancatTheme.colors.text2,
    size: Dp = 40.dp,
    iconSize: Dp = 22.dp,
    enabled: Boolean = true,
) {
    val src = remember { MutableInteractionSource() }
    val pressed by src.collectIsPressedAsState()
    val s by animateFloatAsState(if (pressed) 0.88f else 1f, tween(LancatMotion.DUR, easing = LancatMotion.EaseSpring), label = "press")
    Box(
        modifier
            .size(size)
            .graphicsLayer { scaleX = s; scaleY = s; alpha = if (enabled) 1f else 0.4f }
            .clickable(src, ripple(bounded = false, radius = size / 2), enabled = enabled, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, contentDescription, tint = tint, modifier = Modifier.size(iconSize))
    }
}

/** 顶栏：状态栏留白 + 56dp 内容 + 底部 0.5dp 分隔线。 */
@Composable
fun LcTopBar(
    modifier: Modifier = Modifier,
    navigation: (@Composable () -> Unit)? = null,
    actions: @Composable RowScope.() -> Unit = {},
    title: @Composable BoxScope.() -> Unit,
) {
    val c = LancatTheme.colors
    Column(
        modifier
            .fillMaxWidth()
            .background(c.surface)
            .windowInsetsPadding(WindowInsets.statusBars)
            .drawBehind {
                drawLine(c.sep, Offset(0f, size.height), Offset(size.width, size.height), 1f)
            }
    ) {
        Row(Modifier.fillMaxWidth().height(56.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            if (navigation != null) navigation() else Spacer(Modifier.width(12.dp))
            Box(Modifier.weight(1f), contentAlignment = Alignment.CenterStart, content = title)
            actions()
        }
    }
}

@Composable
fun TitleText(text: String, sub: String? = null, subColor: Color = LancatTheme.colors.text2) {
    val c = LancatTheme.colors
    Column {
        Text(text, style = LancatTheme.type.title, color = c.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (sub != null) Text(sub, style = LancatTheme.type.caption, color = subColor, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

@Composable
fun Hairline(modifier: Modifier = Modifier, start: Dp = 0.dp) {
    val c = LancatTheme.colors
    Box(modifier.fillMaxWidth().padding(start = start).height(0.5.dp).background(c.sep))
}
