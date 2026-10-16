package fan.lancat.ui.fake

import fan.lancat.model.AppRelease
import fan.lancat.model.AppUpdater
import fan.lancat.model.UpdateState
import kotlinx.coroutines.flow.MutableStateFlow

/** 截图 / 预览用的更新器：状态直接给定，按钮只改状态。 */
class FakeAppUpdater(
    initial: UpdateState = UpdateState.Idle,
    dialog: Boolean = false,
    override val currentVersion: String = "0.2.8-android.1",
) : AppUpdater {
    override val state = MutableStateFlow(initial)
    override val dialogVisible = MutableStateFlow(dialog)
    override val autoCheck = MutableStateFlow(true)
    override fun setAutoCheck(on: Boolean) { autoCheck.value = on }
    override fun check(manual: Boolean) { state.value = UpdateState.UpToDate(currentVersion) }
    override fun download() { (state.value as? UpdateState.Available)?.let { state.value = UpdateState.Downloading(it.release, 0, it.release.apkSize) } }
    override fun install() {}
    override fun hideDialog() { dialogVisible.value = false }
    override fun skip() { dialogVisible.value = false }

    companion object {
        val SAMPLE = AppRelease(
            version = "0.2.9",
            notes = "## What's Changed\n* 安卓：应用内更新、项目图标\n* 聊天：消息少时从顶部排列\n* 修复若干问题",
            page = "https://github.com/motto1/Lancat/releases/tag/v0.2.9",
            apkName = "Lancat-android.apk",
            apkUrl = "https://github.com/motto1/Lancat/releases/download/v0.2.9/Lancat-android.apk",
            apkSize = 31_457_280,
            sha256 = "0".repeat(64),
        )
    }
}
