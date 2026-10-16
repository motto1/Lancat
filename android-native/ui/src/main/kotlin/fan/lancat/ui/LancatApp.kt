package fan.lancat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fan.lancat.model.LancatRepository
import fan.lancat.ui.components.Flash
import fan.lancat.ui.components.FlashHost
import fan.lancat.ui.components.LocalFlash
import fan.lancat.ui.nav.LancatNavHost
import fan.lancat.ui.nav.LocalNavigator
import fan.lancat.ui.nav.Screen
import fan.lancat.ui.nav.rememberNavigator
import fan.lancat.ui.screens.PeerListScreen
import fan.lancat.ui.screens.SettingsScreen
import fan.lancat.ui.screens.chat.ChatScreen
import fan.lancat.ui.screens.chat.PeerPickList
import fan.lancat.ui.theme.LancatTheme
import kotlinx.coroutines.launch

/**
 * :ui 对外唯一入口。单 Activity 里直接 setContent { LancatApp(repo, startPeerId) }。
 *
 * 宿主要求（C 组 MainActivity）：
 *  - enableEdgeToEdge()，windowSoftInputMode="adjustResize"（表情 / 键盘无缝切换依赖 ime inset）
 *  - AndroidManifest 里 android:enableOnBackInvokedCallback="true"（Android 13+ 预测性返回）
 *
 * @param startPeerId 从通知 / 快捷方式进来时直接打开的会话；"*" 表示全部设备。变化时会重新导航。
 */
@Composable
fun LancatApp(repo: LancatRepository, startPeerId: String? = null) {
    val settings by repo.settings.collectAsStateWithLifecycle()
    LancatTheme(settings.theme) {
        val nav = rememberNavigator(*(if (startPeerId != null) arrayOf(Screen.Peers, Screen.Chat(startPeerId)) else arrayOf(Screen.Peers)))
        LaunchedEffect(startPeerId) {
            if (startPeerId != null && nav.top != Screen.Chat(startPeerId)) nav.resetTo(Screen.Peers, Screen.Chat(startPeerId))
        }
        val flash = remember { Flash() }
        CompositionLocalProvider(LocalNavigator provides nav, LocalFlash provides flash) {
            Box(Modifier.fillMaxSize().background(LancatTheme.colors.background)) {
                LancatNavHost(nav) { screen ->
                    when (screen) {
                        Screen.Peers -> PeerListScreen(
                            repo,
                            onOpen = { nav.push(Screen.Chat(it)) },
                            onSettings = { nav.push(Screen.Settings) },
                        )
                        is Screen.Chat -> ChatScreen(repo, screen.peerId, onBack = { nav.pop() })
                        Screen.Settings -> SettingsScreen(repo, onBack = { nav.pop() })
                    }
                }
                FlashHost(flash)
            }
        }
    }
}

/**
 * 系统分享进来时的目标选择页（C 组的分享 Activity 里 setContent 调用）。
 * 选好设备后调用 repo.sendFiles(peers, uris)，然后 onDone()。
 */
@Composable
fun ShareSheetContent(repo: LancatRepository, uris: List<String>, onDone: () -> Unit) {
    val settings by repo.settings.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    LancatTheme(settings.theme) {
        val c = LancatTheme.colors
        val flash = remember { Flash() }
        CompositionLocalProvider(LocalFlash provides flash) {
            Column(
                Modifier.fillMaxSize().background(c.surface)
                    .windowInsetsPadding(WindowInsets.statusBars)
                    .windowInsetsPadding(WindowInsets.navigationBars)
            ) {
                Text(
                    if (uris.size == 1) "发送 1 个文件" else "发送 ${uris.size} 个文件",
                    style = LancatTheme.type.caption, color = c.text2,
                    modifier = Modifier.fillMaxWidth().padding(start = 20.dp, top = 14.dp),
                )
                PeerPickList(
                    repo = repo,
                    title = "发送到…",
                    action = "发送",
                    exclude = null,
                    onPick = { picked ->
                        scope.launch {
                            repo.sendFiles(picked, uris)
                            onDone()
                        }
                    },
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}
