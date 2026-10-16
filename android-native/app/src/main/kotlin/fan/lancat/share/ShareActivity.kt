package fan.lancat.share

import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.ui.graphics.Color
import fan.lancat.Bridge
import fan.lancat.R
import fan.lancat.ServiceLocator
import fan.lancat.service.LancatService

/**
 * 系统分享入口（ACTION_SEND / SEND_MULTIPLE，任意 mime 和文字）。
 *
 * - 普通分享：透明窗口里弹半屏 ShareSheetContent，用户选设备后发送。
 * - Direct Share（带 shortcut id）：不弹界面，直接交给 LancatService 发送，只弹一个 toast，进度走通知。
 *   注：Google 的 Direct Share 指南建议点了快捷方式要进到会话界面；这里按产品要求直接发送（STATUS-c.md 已注明）。
 *
 * 发送一律经 LancatService（见 ShareStaging 注释）：uri 授权随 Intent 转给前台服务，
 * 本 Activity 随后 finish 也不影响读取。
 */
class ShareActivity : ComponentActivity() {

    @OptIn(ExperimentalMaterial3Api::class)
    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        val payload = ShareIntentParser.parse(intent, packageName)
        if (payload.isEmpty) {
            Toast.makeText(this, R.string.share_nothing, Toast.LENGTH_SHORT).show()
            finish()
            return
        }

        val repo = ServiceLocator.repo
        val directPeer = payload.shortcutPeerId
        if (directPeer != null && savedInstanceState == null) {
            val name = repo.peers.value.firstOrNull { it.id == directPeer }?.name
            LancatService.send(this, listOf(directPeer), payload.uris, payload.text)
            Toast.makeText(
                this,
                if (name != null) getString(R.string.share_sending_to, name) else getString(R.string.share_sending),
                Toast.LENGTH_SHORT,
            ).show()
            finish()
            return
        }

        val routing = ShareRoutingRepository(repo, applicationContext, payload)
        val uriStrings = payload.uris.map { it.toString() }
        setContent {
            val dark = isSystemInDarkTheme()
            val accent = Color(0xFF0A84FF)
            MaterialTheme(colorScheme = if (dark) darkColorScheme(primary = accent) else lightColorScheme(primary = accent)) {
                val state = rememberModalBottomSheetState(skipPartiallyExpanded = false)
                ModalBottomSheet(onDismissRequest = { finish() }, sheetState = state) {
                    Bridge.ShareSheet(routing, uriStrings, onDone = { finish() })
                }
            }
        }
    }

    override fun finish() {
        super.finish()
        @Suppress("DEPRECATION")
        overridePendingTransition(0, 0)
    }
}
