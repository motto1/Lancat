package fan.lancat.model

import kotlinx.coroutines.flow.StateFlow

/**
 * 应用内更新（对齐桌面 v0.2.8 的 update.rs：GitHub Releases + SHA-256 校验）。
 * :app 的 fan.lancat.update.Updater 实现；:ui 只依赖这个接口（设置页「检查更新」、新版本对话框）。
 */
data class AppRelease(
    /** 不带 v 的版本号，如 "0.2.9" / "0.2.8-android.2" */
    val version: String,
    val notes: String,
    val page: String,
    val apkName: String,
    val apkUrl: String,
    val apkSize: Long,
    /** 小写十六进制；发布里没有校验信息时为 null（不会安装没法校验的包） */
    val sha256: String?,
)

sealed interface UpdateState {
    data object Idle : UpdateState
    data object Checking : UpdateState
    data class UpToDate(val latest: String?) : UpdateState
    data class Available(val release: AppRelease) : UpdateState
    data class Downloading(val release: AppRelease, val bytes: Long, val total: Long) : UpdateState
    data class Verifying(val release: AppRelease) : UpdateState
    /** 已下载并校验；needsPermission = 还没允许「安装未知应用」 */
    data class ReadyToInstall(val release: AppRelease, val needsPermission: Boolean) : UpdateState
    data class Failed(val message: String, val release: AppRelease?) : UpdateState
}

interface AppUpdater {
    /** 当前安装的版本（versionName） */
    val currentVersion: String
    val state: StateFlow<UpdateState>
    /** 新版本对话框是否显示（自动检查发现新版、手动检查、下载 / 安装过程中） */
    val dialogVisible: StateFlow<Boolean>
    /** 启动时 / 每 6 小时自动检查 */
    val autoCheck: StateFlow<Boolean>

    fun setAutoCheck(on: Boolean)
    /** manual = 用户点了「检查更新」：结果（含已是最新 / 失败）都要反馈并打开对话框 */
    fun check(manual: Boolean)
    fun download()
    /** 安装已校验的包；没有「安装未知应用」权限时先跳到系统授权页 */
    fun install()
    /** 关闭对话框（下载继续在后台进行，通知栏有进度） */
    fun hideDialog()
    /** 「跳过此版本」：自动检查不再提示这个版本（手动检查仍会显示） */
    fun skip()
}

/**
 * 版本号比较。规则：
 *  - 主体 x.y.z 按数字比较（缺省补 0，前缀 v 忽略）；
 *  - 安卓修订后缀 "-android.N" 比同主体的正式版更新（0.2.8-android.1 > 0.2.8），N 按数字比较；
 *  - 其他预发布后缀（-beta、-rc.1 …）比同主体正式版更旧，彼此按字符串比较；
 *  - "+build" 元数据忽略。解析失败返回 null。
 */
object AppVersion {
    data class Parsed(val major: Long, val minor: Long, val patch: Long, val android: Long, val pre: String?) : Comparable<Parsed> {
        override fun compareTo(other: Parsed): Int = compareValuesBy(this, other,
            { it.major }, { it.minor }, { it.patch },
            { if (it.pre != null) 0 else 1 },        // 预发布 < 正式
            { it.pre ?: "" },
            { it.android },
        )
    }

    fun parse(v: String): Parsed? {
        val s = v.trim().removePrefix("v").removePrefix("V").substringBefore('+')
        if (s.isEmpty()) return null
        val core = s.substringBefore('-')
        val suffix = if ('-' in s) s.substringAfter('-') else null
        val nums = core.split('.')
        if (nums.size > 3) return null
        val n = nums.map { it.toLongOrNull() ?: return null }
        var android = 0L
        var pre: String? = null
        if (suffix != null) {
            val m = Regex("""^android\.?(\d+)$""", RegexOption.IGNORE_CASE).find(suffix)
            if (m != null) android = m.groupValues[1].toLong() else pre = suffix
        }
        return Parsed(n[0], n.getOrElse(1) { 0 }, n.getOrElse(2) { 0 }, android, pre)
    }

    fun isNewer(remote: String, local: String): Boolean {
        val r = parse(remote) ?: return false
        val l = parse(local) ?: return true            // 本地版本号认不出（dev 构建）：有发布就算新
        return r > l
    }

    /** 由版本号推出 versionCode：(x·10000 + y·100 + z)·100 + 安卓修订号，保证随版本单调递增。 */
    fun versionCode(v: String): Int? {
        val p = parse(v) ?: return null
        if (p.pre != null || p.minor > 99 || p.patch > 99 || p.android > 99) return null
        return (((p.major * 10000 + p.minor * 100 + p.patch) * 100) + p.android).toInt()
    }
}
