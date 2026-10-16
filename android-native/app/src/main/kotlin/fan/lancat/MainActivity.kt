package fan.lancat

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.mutableStateOf
import fan.lancat.perm.Permissions
import fan.lancat.service.NetPresence

/**
 * 唯一的 Activity。edge-to-edge，整个界面交给 :ui 的 LancatApp(repo, startPeerId)。
 * 通知、动态快捷方式点进来时带 [EXTRA_PEER_ID]，singleTask 下走 onNewIntent，更新 startPeerId 让 UI 跳到对应会话。
 */
class MainActivity : ComponentActivity() {
    private val startPeerId = mutableStateOf<String?>(null)
    private lateinit var permissions: Permissions

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        permissions = Permissions(this)
        startPeerId.value = peerIdFrom(intent)
        val repo = ServiceLocator.repo
        setContent {
            Bridge.AppRoot(repo, startPeerId.value)
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        peerIdFrom(intent)?.let { startPeerId.value = it }
    }

    override fun onStart() {
        super.onStart()
        NetPresence.acquire("ui")
        permissions.requestStartupPermissions()
    }

    override fun onStop() {
        NetPresence.release("ui")
        super.onStop()
    }

    companion object {
        const val EXTRA_PEER_ID = "fan.lancat.extra.PEER_ID"

        fun peerIdFrom(intent: Intent?): String? = intent?.getStringExtra(EXTRA_PEER_ID)?.takeIf { it.isNotBlank() }
    }
}
