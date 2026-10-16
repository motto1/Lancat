package fan.lancat

import android.content.Context
import androidx.compose.runtime.Composable
import fan.lancat.model.LancatRepository
import fan.lancat.stub.LancatApp
import fan.lancat.stub.LancatCoreRepository
import fan.lancat.stub.ShareSheetContent

/**
 * :app 与 :core / :ui 之间唯一的接缝。这一份连的是 fan.lancat.stub 占位实现；
 * 集成时 build.gradle.kts 切到 src/real/kotlin/fan/lancat/Bridge.kt，其余代码不用改。
 */
internal object Bridge {
    fun createRepository(context: Context): LancatRepository = LancatCoreRepository(context)

    /** 网络变化（Wi-Fi 切换、IP 变化）：让 core 重新绑定套接字并重连 */
    fun onNetworkChanged(repo: LancatRepository) {
        (repo as? LancatCoreRepository)?.onNetworkChanged()
    }

    @Composable
    fun AppRoot(repo: LancatRepository, startPeerId: String?) = LancatApp(repo, startPeerId)

    @Composable
    fun ShareSheet(repo: LancatRepository, uris: List<String>, onDone: () -> Unit) =
        ShareSheetContent(repo, uris, onDone)
}
