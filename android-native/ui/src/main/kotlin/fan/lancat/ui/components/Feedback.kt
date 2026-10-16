package fan.lancat.ui.components

import android.content.ClipboardManager
import android.content.Context
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import fan.lancat.ui.theme.LancatShapes
import fan.lancat.ui.theme.LancatTheme
import kotlinx.coroutines.delay

/** 轻提示（桌面 app.flash）：底部居中一枚胶囊，2.2 秒后消失。 */
@Stable
class Flash {
    var text by mutableStateOf<String?>(null)
        private set
    internal var serial by mutableIntStateOf(0)
    fun show(t: String) { text = t; serial++ }
    internal fun clear() { text = null }
}

val LocalFlash = staticCompositionLocalOf { Flash() }

@Composable
fun FlashHost(flash: Flash, modifier: Modifier = Modifier) {
    LaunchedEffect(flash.serial) {
        if (flash.text != null) { delay(2200); flash.clear() }
    }
    Box(modifier.fillMaxSize().windowInsetsPadding(WindowInsets.navigationBars), contentAlignment = Alignment.BottomCenter) {
        AnimatedVisibility(flash.text != null, enter = fadeIn() + scaleIn(initialScale = 0.9f), exit = fadeOut() + scaleOut(targetScale = 0.95f)) {
            val t = flash.text ?: ""
            Text(
                t,
                color = Color.White,
                style = LancatTheme.type.bodySmall,
                modifier = Modifier
                    .padding(bottom = 96.dp)
                    .background(Color(0xE6303034), LancatShapes.pill)
                    .padding(horizontal = 16.dp, vertical = 9.dp),
            )
        }
    }
}

/** 剪贴板内容：文字或若干 content:// uri。 */
sealed interface ClipContent {
    data class Text(val text: String) : ClipContent
    data class Uris(val uris: List<String>) : ClipContent
}

fun readClipboard(context: Context): ClipContent? {
    val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return null
    val clip = runCatching { cm.primaryClip }.getOrNull() ?: return null
    val uris = (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri?.toString() }
    if (uris.isNotEmpty()) return ClipContent.Uris(uris)
    val text = (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).coerceToText(context)?.toString() }
        .joinToString("\n").trim()
    return if (text.isEmpty()) null else ClipContent.Text(text)
}

fun copyToClipboard(context: Context, text: String) {
    val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return
    cm.setPrimaryClip(android.content.ClipData.newPlainText("Lancat", text))
}
