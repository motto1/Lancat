package fan.lancat

import android.content.Context
import androidx.compose.runtime.Composable
import fan.lancat.core.LancatCoreRepository
import fan.lancat.model.LancatRepository
import fan.lancat.ui.LancatApp
import fan.lancat.ui.ShareSheetContent

/** 集成版接缝：:core 的真实实现 + :ui 的真实页面。签名以契约为准，若 A/B 组实际签名不同只改这一个文件。 */
internal object Bridge {
    fun createRepository(context: Context): LancatRepository = LancatCoreRepository(context)

    fun onNetworkChanged(repo: LancatRepository) {
        (repo as? LancatCoreRepository)?.onNetworkChanged()
    }

    @Composable
    fun AppRoot(repo: LancatRepository, startPeerId: String?) = LancatApp(repo, startPeerId, fan.lancat.update.Updater)

    @Composable
    fun ShareSheet(repo: LancatRepository, uris: List<String>, onDone: () -> Unit) =
        ShareSheetContent(repo, uris, onDone)
}
