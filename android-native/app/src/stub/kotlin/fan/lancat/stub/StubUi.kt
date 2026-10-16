package fan.lancat.stub

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import fan.lancat.model.LancatRepository
import kotlinx.coroutines.launch

/** 占位：:ui 的 LancatApp(repo, startPeerId)。只列出设备，证明注入链路通了。 */
@Composable
fun LancatApp(repo: LancatRepository, startPeerId: String?) {
    val peers by repo.peers.collectAsState()
    val self by repo.self.collectAsState()
    MaterialTheme {
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.safeDrawingPadding().padding(16.dp)) {
                Text("Lancat（占位界面）", style = MaterialTheme.typography.titleLarge)
                Text("本机：${self?.name ?: "-"}  startPeerId=${startPeerId ?: "-"}")
                Spacer(Modifier.height(12.dp))
                if (peers.isEmpty()) Text("还没有发现设备")
                LazyColumn { items(peers, key = { it.id }) { Text("${it.name}  ${if (it.online) "在线" else "离线"}") } }
            }
        }
    }
}

/** 占位：:ui 的 ShareSheetContent(repo, uris, onDone)。点一台设备就发送并结束。 */
@Composable
fun ShareSheetContent(repo: LancatRepository, uris: List<String>, onDone: () -> Unit) {
    val peers by repo.peers.collectAsState()
    val scope = rememberCoroutineScope()
    Column(Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp)) {
        Text("发送 ${uris.size} 项到…", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(8.dp))
        if (peers.isEmpty()) Text("还没有发现设备", Modifier.clickable { onDone() }.padding(vertical = 12.dp))
        peers.filter { !it.blocked }.forEach { p ->
            Text(
                p.name,
                Modifier.fillMaxWidth().clickable {
                    scope.launch { repo.sendFiles(listOf(p.id), uris); onDone() }
                }.padding(vertical = 12.dp),
            )
        }
    }
}
