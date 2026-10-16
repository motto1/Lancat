package fan.lancat.ui.theme

import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * 视觉 token，取自桌面端 src/styles/global.css 与 crates/lancat-slint/ui/app.slint 的 Theme，
 * 半透明的玻璃色按桌面截图（work/slint/ref/old-*.png）实测压成不透明色，移动端没有亚克力底。
 */
@Immutable
data class LancatColors(
    val isDark: Boolean,
    val accent: Color,
    val accentContrast: Color,
    /** 聊天区底色（桌面 chat 区） */
    val background: Color,
    /** 顶栏 / 输入栏 / 底部抽屉（桌面 header / glass-strong） */
    val surface: Color,
    /** 会话列表底色（桌面 sidebar） */
    val list: Color,
    /** 卡片、设置分组（桌面 glass-strong） */
    val card: Color,
    val text: Color,
    val text2: Color,
    val text3: Color,
    val sep: Color,
    val bubbleIn: Color,
    val bubbleOut: Color,
    val hover: Color,
    val field: Color,
    val online: Color,
    val danger: Color,
    val scrim: Color,
    val datePill: Color,
)

val LightLancatColors = LancatColors(
    isDark = false,
    accent = Color(0xFF0A84FF),
    accentContrast = Color.White,
    background = Color(0xFFF6F6F8),
    surface = Color(0xFFF8F8FA),
    list = Color(0xFFFCFCFD),
    card = Color(0xFFFFFFFF),
    text = Color(0xFF1D1D1F),
    text2 = Color(0xB83C3C43),   // rgba(60,60,67,.72)
    text3 = Color(0x733C3C43),   // rgba(60,60,67,.45)
    sep = Color(0x14000000),     // rgba(0,0,0,.08)
    bubbleIn = Color(0xFFE7E7E9),
    bubbleOut = Color(0xFF0A84FF),
    hover = Color(0x0D000000),
    field = Color(0xFFFFFFFF),
    online = Color(0xFF30D158),
    danger = Color(0xFFFF3B30),
    scrim = Color(0x33000000),
    datePill = Color(0x99707579),
)

val DarkLancatColors = LancatColors(
    isDark = true,
    accent = Color(0xFF0A84FF),
    accentContrast = Color.White,
    background = Color(0xFF1E1E21),
    surface = Color(0xFF2A2A2D),
    list = Color(0xFF232326),
    card = Color(0xFF2C2C30),
    text = Color(0xFFF5F5F7),
    text2 = Color(0xADEBEBF5),   // rgba(235,235,245,.68)
    text3 = Color(0x61EBEBF5),   // rgba(235,235,245,.38)
    sep = Color(0x17FFFFFF),     // rgba(255,255,255,.09)
    bubbleIn = Color(0xFF38383B),
    bubbleOut = Color(0xFF0A84FF),
    hover = Color(0x12FFFFFF),
    field = Color(0xFF2D2D30),
    online = Color(0xFF30D158),
    danger = Color(0xFFFF453A),
    scrim = Color(0x66000000),
    datePill = Color(0x99000000),
)

@Immutable
object LancatShapes {
    val bubbleRadius = 18.dp
    val bubbleTail = 6.dp
    val cardRadius = 14.dp
    val card = RoundedCornerShape(14.dp)
    val small = RoundedCornerShape(9.dp)
    val pill = RoundedCornerShape(50)
}

@Immutable
data class LancatType(
    val title: TextStyle = TextStyle(fontSize = 17.sp, fontWeight = FontWeight.SemiBold, lineHeight = 22.sp),
    val name: TextStyle = TextStyle(fontSize = 16.sp, fontWeight = FontWeight.SemiBold, lineHeight = 21.sp),
    val body: TextStyle = TextStyle(fontSize = 16.sp, lineHeight = 22.sp),
    val bodySmall: TextStyle = TextStyle(fontSize = 14.sp, lineHeight = 19.sp),
    val caption: TextStyle = TextStyle(fontSize = 12.sp, lineHeight = 16.sp),
    val micro: TextStyle = TextStyle(fontSize = 11.sp, lineHeight = 13.sp),
    val section: TextStyle = TextStyle(fontSize = 13.sp, fontWeight = FontWeight.Medium, lineHeight = 18.sp),
)

/** 动效 token：与桌面 --dur-* / --ease* 一致，转场曲线取 Telegram 的 EASE_OUT_QUINT。 */
object LancatMotion {
    const val DUR_FAST = 120
    const val DUR = 160
    const val DUR_SLOW = 220
    const val PAGE = 300
    val Ease = CubicBezierEasing(0.2f, 0.8f, 0.3f, 1f)
    val EaseSpring = CubicBezierEasing(0.2f, 0.9f, 0.3f, 1.4f)
    /** Telegram CubicBezierInterpolator.EASE_OUT_QUINT */
    val EaseOutQuint = CubicBezierEasing(0.23f, 1f, 0.32f, 1f)
    /** Telegram CubicBezierInterpolator.DEFAULT */
    val TgDefault = CubicBezierEasing(0.25f, 0.1f, 0.25f, 1f)
    /** 消息入场：桌面 msg-in 320ms cubic-bezier(0.2,0.9,0.3,1.15) */
    val MsgIn = CubicBezierEasing(0.2f, 0.9f, 0.3f, 1.15f)
}

val LocalLancatColors = staticCompositionLocalOf { LightLancatColors }
val LocalLancatType = staticCompositionLocalOf { LancatType() }

object LancatTheme {
    val colors: LancatColors
        @Composable @ReadOnlyComposable get() = LocalLancatColors.current
    val type: LancatType
        @Composable @ReadOnlyComposable get() = LocalLancatType.current
    val shapes: LancatShapes get() = LancatShapes
}

/** 解析设置项 theme（"system" | "light" | "dark"）。 */
@Composable
fun resolveDark(theme: String): Boolean = when (theme) {
    "light" -> false
    "dark" -> true
    else -> isSystemInDarkTheme()
}

@Composable
fun LancatTheme(theme: String = "system", content: @Composable () -> Unit) {
    LancatTheme(dark = resolveDark(theme), content = content)
}

@Composable
fun LancatTheme(dark: Boolean, content: @Composable () -> Unit) {
    val c = if (dark) DarkLancatColors else LightLancatColors
    val scheme = if (dark) {
        darkColorScheme(
            primary = c.accent, onPrimary = c.accentContrast, background = c.background, onBackground = c.text,
            surface = c.surface, onSurface = c.text, surfaceVariant = c.field, onSurfaceVariant = c.text2,
            surfaceContainer = c.card, surfaceContainerHigh = c.card, surfaceContainerLow = c.surface,
            outline = c.sep, outlineVariant = c.sep, error = c.danger,
        )
    } else {
        lightColorScheme(
            primary = c.accent, onPrimary = c.accentContrast, background = c.background, onBackground = c.text,
            surface = c.surface, onSurface = c.text, surfaceVariant = c.field, onSurfaceVariant = c.text2,
            surfaceContainer = c.card, surfaceContainerHigh = c.card, surfaceContainerLow = c.surface,
            outline = c.sep, outlineVariant = c.sep, error = c.danger,
        )
    }
    CompositionLocalProvider(LocalLancatColors provides c, LocalLancatType provides LancatType()) {
        MaterialTheme(colorScheme = scheme, content = content)
    }
}
