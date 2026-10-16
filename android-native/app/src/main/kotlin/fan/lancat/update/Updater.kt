package fan.lancat.update

import android.annotation.SuppressLint
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.FileProvider
import fan.lancat.AppForeground
import fan.lancat.BuildConfig
import fan.lancat.MainActivity
import fan.lancat.R
import fan.lancat.model.AppRelease
import fan.lancat.model.AppUpdater
import fan.lancat.model.AppVersion
import fan.lancat.model.UpdateState
import fan.lancat.notify.Notify
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest

/**
 * 应用内更新，对齐桌面 update.rs：
 *  - 检查：GitHub Releases（motto1/Lancat），挑带 `Lancat-android.apk` 的最新正式发布，按版本号比较（[AppVersion]）。
 *    启动 / 回到前台时若距上次检查超过 6 小时就查一次（设置里可关）；设置页「检查更新」随时手动查。
 *  - 下载：应用内下载到 cache/updates，通知栏显示进度；必须有 SHA-256（.sha256 / SHA256SUMS / GitHub digest）且一致，
 *    并核对包名、versionCode 更大、签名证书与已安装的一致，才交给系统安装。
 *  - 安装：FileProvider + ACTION_VIEW 交给系统安装器；Android 8+ 先确认「安装未知应用」权限，没有就跳到系统授权页，
 *    回到应用（onStart）后自动继续。
 */
@SuppressLint("StaticFieldLeak")
object Updater : AppUpdater {
    private const val TAG = "LancatUpdate"
    const val REPO = "motto1/Lancat"
    private const val API = "https://api.github.com/repos/$REPO/releases?per_page=20"
    private const val AUTO_INTERVAL_MS = 6 * 3600_000L
    const val CHANNEL = "upd"
    private const val NOTIF_ID = 7
    const val EXTRA_INSTALL = "fan.lancat.extra.INSTALL_UPDATE"

    private lateinit var app: Context
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var job: Job? = null
    @Volatile private var pendingInstall = false
    private var apkFile: File? = null

    override val currentVersion: String get() = BuildConfig.VERSION_NAME
    private val _state = MutableStateFlow<UpdateState>(UpdateState.Idle)
    override val state: StateFlow<UpdateState> = _state.asStateFlow()
    private val _dialog = MutableStateFlow(false)
    override val dialogVisible: StateFlow<Boolean> = _dialog.asStateFlow()
    private val _auto = MutableStateFlow(true)
    override val autoCheck: StateFlow<Boolean> = _auto.asStateFlow()

    private val prefs get() = app.getSharedPreferences("update", Context.MODE_PRIVATE)

    fun init(ctx: Context) {
        app = ctx.applicationContext
        _auto.value = prefs.getBoolean("auto", true)
        if (Build.VERSION.SDK_INT >= 26) {
            app.getSystemService(NotificationManager::class.java)?.createNotificationChannel(
                NotificationChannel(CHANNEL, app.getString(R.string.ch_upd), NotificationManager.IMPORTANCE_LOW).apply {
                    description = app.getString(R.string.ch_upd_desc); setShowBadge(false)
                }
            )
        }
        // 半截下载和已经装上（不比当前新）的旧安装包没用了
        scope.launch {
            dir().listFiles()?.forEach { f ->
                val v = f.name.removeSuffix(".apk")
                if (!f.name.endsWith(".apk") || !AppVersion.isNewer(v, currentVersion)) f.delete()
            }
        }
    }

    private fun dir() = File(app.cacheDir, "updates").apply { mkdirs() }

    /** MainActivity.onStart：到点自动检查；从「安装未知应用」授权页回来时继续安装 */
    fun onForeground() {
        if (!::app.isInitialized) return
        val s = _state.value
        if (pendingInstall && s is UpdateState.ReadyToInstall) {
            if (canInstall()) { pendingInstall = false; _state.value = s.copy(needsPermission = false); launchInstaller() }
            else _state.value = s.copy(needsPermission = true)
            return
        }
        if (_auto.value && System.currentTimeMillis() - prefs.getLong("last", 0) > AUTO_INTERVAL_MS) check(manual = false)
    }

    override fun setAutoCheck(on: Boolean) {
        _auto.value = on
        prefs.edit().putBoolean("auto", on).apply()
    }

    override fun check(manual: Boolean) {
        val busy = _state.value
        if (busy is UpdateState.Checking || busy is UpdateState.Downloading || busy is UpdateState.Verifying) {
            if (manual && busy !is UpdateState.Checking) _dialog.value = true
            return
        }
        // 已经发现 / 下载好的新版本：手动「查看」直接打开对话框
        if (manual && (busy is UpdateState.Available || busy is UpdateState.ReadyToInstall)) { _dialog.value = true; return }
        job = scope.launch {
            _state.value = UpdateState.Checking
            val r = runCatching { fetchLatest() }
            prefs.edit().putLong("last", System.currentTimeMillis()).apply()
            r.onFailure { e ->
                Log.w(TAG, "check failed", e)
                _state.value = if (manual) UpdateState.Failed(if (e is UpdateError) e.message!! else netMsg(e), null) else UpdateState.Idle
            }
            r.onSuccess { rel ->
                if (rel == null || !AppVersion.isNewer(rel.version, currentVersion)) {
                    _state.value = UpdateState.UpToDate(rel?.version)
                } else {
                    // 已经下载校验过的同一版本：直接进入待安装
                    val f = File(dir(), "${rel.version}.apk")
                    if (f.exists() && rel.sha256 != null && sha256(f) == rel.sha256) {
                        apkFile = f; _state.value = UpdateState.ReadyToInstall(rel, !canInstall())
                    } else {
                        _state.value = UpdateState.Available(rel)
                    }
                    val skipped = prefs.getString("skip", null) == rel.version
                    if (manual || !skipped) _dialog.value = true
                }
            }
        }
    }

    private fun netMsg(e: Throwable): String = when (e) {
        is java.net.UnknownHostException -> "无法连接 GitHub（网络不可用或域名解析失败）"
        is java.net.SocketTimeoutException -> "连接 GitHub 超时"
        is IOException -> e.message?.let { "网络请求失败：$it" } ?: "网络请求失败"
        else -> e.message ?: e.toString()
    }

    private fun open(url: String, accept: String? = null): HttpURLConnection {
        val c = URL(url).openConnection() as HttpURLConnection
        c.connectTimeout = 15_000; c.readTimeout = 30_000; c.instanceFollowRedirects = true
        c.setRequestProperty("User-Agent", "Lancat-Android/$currentVersion")
        if (accept != null) c.setRequestProperty("Accept", accept)
        return c
    }

    private fun getText(url: String, accept: String? = null): String {
        val c = open(url, accept)
        try {
            if (c.responseCode == 403 || c.responseCode == 429) throw UpdateError("GitHub 接口访问次数超限，请稍后再试")
            if (c.responseCode !in 200..299) throw IOException("HTTP ${c.responseCode}")
            return c.inputStream.use { it.readBytes().toString(Charsets.UTF_8) }
        } finally { c.disconnect() }
    }

    /** 最新的带安卓包的正式发布（含校验值）；没有则 null */
    private fun fetchLatest(): AppRelease? {
        val cand = ReleaseParser.pickLatest(getText(API, "application/vnd.github+json")) ?: return null
        var rel = cand.release
        if (cand.shaFileUrl != null) {
            val sha = runCatching { ReleaseParser.shaFromText(getText(cand.shaFileUrl), rel.apkName, cand.shaFileIsSums) }.getOrNull()
            if (sha != null) rel = rel.copy(sha256 = sha)
        }
        return rel
    }

    override fun download() {
        val rel = when (val s = _state.value) {
            is UpdateState.Available -> s.release
            is UpdateState.Failed -> s.release ?: return check(manual = true)
            is UpdateState.ReadyToInstall -> return install()
            else -> return
        }
        _dialog.value = true
        job = scope.launch {
            try {
                val sha = rel.sha256 ?: throw UpdateError("这个版本没有提供 SHA-256 校验值，为安全起见不安装")
                val out = File(dir(), "${rel.version}.apk")
                val part = File(dir(), "${rel.version}.apk.part")
                _state.value = UpdateState.Downloading(rel, 0, rel.apkSize)
                notifyProgress(rel, 0, rel.apkSize)
                val c = open(rel.apkUrl, "application/octet-stream")
                try {
                    if (c.responseCode !in 200..299) throw UpdateError("下载失败：HTTP ${c.responseCode}")
                    val total = c.contentLengthLong.takeIf { it > 0 } ?: rel.apkSize
                    val md = MessageDigest.getInstance("SHA-256")
                    var done = 0L; var lastUi = 0L
                    c.inputStream.use { inp ->
                        part.outputStream().use { o ->
                            val buf = ByteArray(64 * 1024)
                            while (true) {
                                ensureActive()
                                val n = inp.read(buf); if (n < 0) break
                                o.write(buf, 0, n); md.update(buf, 0, n); done += n
                                val t = System.currentTimeMillis()
                                if (t - lastUi > 250) { lastUi = t; _state.value = UpdateState.Downloading(rel, done, total) }
                                if (t - lastNotif > 1000) notifyProgress(rel, done, total)
                            }
                        }
                    }
                    if (rel.apkSize > 0 && done != rel.apkSize) throw UpdateError("下载不完整（$done / ${rel.apkSize} 字节）")
                    _state.value = UpdateState.Verifying(rel)
                    val got = md.digest().joinToString("") { "%02x".format(it) }
                    if (got != sha.lowercase()) { part.delete(); throw UpdateError("SHA-256 校验失败，安装包可能被篡改或损坏，已删除") }
                } finally { c.disconnect() }
                checkPackage(part)
                out.delete(); if (!part.renameTo(out)) throw UpdateError("无法保存安装包")
                apkFile = out
                _state.value = UpdateState.ReadyToInstall(rel, !canInstall())
                if (AppForeground.mainVisible) { cancelNotif(); install() } else notifyReady(rel)
            } catch (e: kotlinx.coroutines.CancellationException) {
                cancelNotif(); throw e
            } catch (e: Throwable) {
                Log.w(TAG, "download failed", e)
                cancelNotif()
                _state.value = UpdateState.Failed(if (e is UpdateError) e.message!! else netMsg(e), rel)
                _dialog.value = true
            }
        }
    }

    /** 包名一致、versionCode 更大、签名证书与已安装的相同（否则系统安装器只会报「与现有软件包冲突」） */
    private fun checkPackage(f: File) {
        val pm = app.packageManager
        val flags = if (Build.VERSION.SDK_INT >= 28) PackageManager.GET_SIGNING_CERTIFICATES else @Suppress("DEPRECATION") PackageManager.GET_SIGNATURES
        val info = pm.getPackageArchiveInfo(f.path, flags) ?: throw UpdateError("下载的文件不是有效的安装包")
        if (info.packageName != app.packageName) throw UpdateError("安装包的包名不对（${info.packageName}）")
        val mine = pm.getPackageInfo(app.packageName, flags)
        if (code(info) <= code(mine)) throw UpdateError("安装包版本不比当前新（${info.versionName}）")
        val a = certs(info); val b = certs(mine)
        if (a.isNotEmpty() && b.isNotEmpty() && a.intersect(b).isEmpty()) throw UpdateError("安装包签名与当前应用不一致，无法覆盖安装")
    }

    @Suppress("DEPRECATION")
    private fun code(p: PackageInfo): Long = if (Build.VERSION.SDK_INT >= 28) p.longVersionCode else p.versionCode.toLong()

    @Suppress("DEPRECATION")
    private fun certs(p: PackageInfo): Set<String> {
        val sigs = if (Build.VERSION.SDK_INT >= 28) p.signingInfo?.let { if (it.hasMultipleSigners()) it.apkContentsSigners else it.signingCertificateHistory } else p.signatures
        val md = MessageDigest.getInstance("SHA-256")
        return sigs.orEmpty().map { s -> md.digest(s.toByteArray()).joinToString("") { "%02x".format(it) } }.toSet()
    }

    fun canInstall(): Boolean = Build.VERSION.SDK_INT < 26 || app.packageManager.canRequestPackageInstalls()

    override fun install() {
        val s = _state.value as? UpdateState.ReadyToInstall ?: return
        if (!canInstall()) {
            pendingInstall = true
            _state.value = s.copy(needsPermission = true)
            val i = Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${app.packageName}")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            runCatching { app.startActivity(i) }.onFailure {
                _state.value = UpdateState.Failed("无法打开「安装未知应用」设置，请在系统设置里允许 Lancat 安装应用", s.release)
            }
            return
        }
        launchInstaller()
    }

    private fun installIntent(): Intent? {
        val f = apkFile ?: (_state.value as? UpdateState.ReadyToInstall)?.let { File(dir(), "${it.release.version}.apk") }
        if (f == null || !f.exists()) return null
        val uri = FileProvider.getUriForFile(app, "${app.packageName}.files", f)
        return Intent(Intent.ACTION_VIEW).setDataAndType(uri, "application/vnd.android.package-archive")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
    }

    private fun launchInstaller() {
        val s = _state.value as? UpdateState.ReadyToInstall ?: return
        val i = installIntent()
        if (i == null) { _state.value = UpdateState.Failed("安装包不见了，请重新下载", s.release); return }
        runCatching { app.startActivity(i) }.onFailure {
            _state.value = UpdateState.Failed("无法启动系统安装器：${it.message}", s.release)
        }
    }

    override fun hideDialog() { _dialog.value = false }

    override fun skip() {
        val v = when (val s = _state.value) {
            is UpdateState.Available -> s.release.version
            is UpdateState.ReadyToInstall -> s.release.version
            is UpdateState.Failed -> s.release?.version
            else -> null
        }
        if (v != null) prefs.edit().putString("skip", v).apply()
        _dialog.value = false
    }

    // ------------------------------------------------------------------ 通知

    @Volatile private var lastNotif = 0L

    private fun base(rel: AppRelease) = NotificationCompat.Builder(app, CHANNEL)
        .setSmallIcon(R.drawable.ic_stat)
        .setColor(0xFFD97757.toInt())
        .setContentTitle("Lancat ${rel.version}")
        .setOnlyAlertOnce(true)
        .setContentIntent(PendingIntent.getActivity(app, 70, Intent(app, MainActivity::class.java).putExtra(EXTRA_INSTALL, true)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))

    private fun notifyProgress(rel: AppRelease, done: Long, total: Long) {
        lastNotif = System.currentTimeMillis()
        val pct = if (total > 0) ((done * 1000) / total).toInt() else 0
        Notify.post(app, null, NOTIF_ID, base(rel).setContentText("正在下载更新…").setOngoing(true)
            .setProgress(1000, pct, total <= 0).setSilent(true).build())
    }

    private fun notifyReady(rel: AppRelease) {
        Notify.post(app, null, NOTIF_ID, base(rel).setContentText("新版本已下载，点按安装").setAutoCancel(true).build())
    }

    private fun cancelNotif() = Notify.cancel(app, null, NOTIF_ID)

    /** 点了「已下载，点按安装」通知（MainActivity 收到 EXTRA_INSTALL） */
    fun onInstallIntent() {
        if (!::app.isInitialized) return
        when (_state.value) {
            is UpdateState.ReadyToInstall -> { _dialog.value = true; install() }
            else -> _dialog.value = true
        }
    }

    private fun sha256(f: File): String {
        val md = MessageDigest.getInstance("SHA-256")
        f.inputStream().use { i -> val b = ByteArray(64 * 1024); while (true) { val n = i.read(b); if (n < 0) break; md.update(b, 0, n) } }
        return md.digest().joinToString("") { "%02x".format(it) }
    }
}

/** 自己判出来的失败（校验、包信息），消息直接给用户看 */
class UpdateError(msg: String) : Exception(msg)
