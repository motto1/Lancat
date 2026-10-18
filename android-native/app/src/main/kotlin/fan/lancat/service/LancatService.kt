package fan.lancat.service

import android.app.ForegroundServiceStartNotAllowedException
import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.IBinder
import android.os.SystemClock
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import fan.lancat.MainActivity
import fan.lancat.R
import fan.lancat.ServiceLocator
import fan.lancat.notify.Channels
import fan.lancat.notify.Notify
import fan.lancat.share.ShareStaging
import fan.lancat.shortcut.ShortcutPublisher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * 前台服务。
 * - 有传输时起、无传输且未开常驻时停（ServiceController + FgsPolicy）；常驻时 connectedDevice，否则 dataSync
 * - 持有 NetPresence("service")：MulticastLock + 网络变化回调；传输期间 TransferLocks（唤醒锁 + Wi-Fi 锁）
 * - ACTION_SEND：分享/Direct Share 的发送入口。uri 通过 ClipData + FLAG_GRANT_READ_URI_PERMISSION 转授给本服务，
 *   服务在所有传输结束前不 stopSelf，授权就一直有效；小文件另复制一份到缓存（ShareStaging）
 * - onTimeout（Android 15+，dataSync 6 小时用完）：几秒内必须停，否则系统抛 RemoteServiceException；
 *   这里发一条「传输已暂停，打开 Lancat 继续」的通知后立刻 stopForeground + stopSelf
 */
class LancatService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var currentTypes = -1
    private var idleJob: Job? = null
    private var foreground = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        running = true
        NetPresence.acquire("service")
        scope.launch {
            ServiceController.state.collect { s ->
                if (foreground) refreshForeground(s)
                scheduleIdleCheck()
            }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // startForegroundService 之后必须尽快 startForeground（5 秒内），每次 start 都调
        if (!goForeground(ServiceController.state.value)) {
            stopSelf()
            return START_NOT_STICKY
        }
        when (intent?.action) {
            ACTION_SEND -> handleSend(intent)
            ACTION_STOP_KEEPALIVE -> scope.launch {
                runCatching { ServiceLocator.repo.updateSettings { it.copy(keepAlive = false) } }
            }
        }
        scheduleIdleCheck()
        // 常驻时被系统杀掉后要求重启；否则不重启（节点随进程起，没有传输就不必保活）
        return if (ServiceController.state.value.keepAlive) START_STICKY else START_NOT_STICKY
    }

    private fun goForeground(s: ServiceController.State): Boolean {
        val types = FgsPolicy.types(s.keepAlive, Build.VERSION.SDK_INT)
        return try {
            ServiceCompat.startForeground(this, NOTIFICATION_ID, buildNotification(s), types)
            currentTypes = types
            foreground = true
            true
        } catch (e: Exception) {
            // Android 12+ 后台启动不允许（ForegroundServiceStartNotAllowedException）、
            // Android 15+ dataSync 额度用完（同一异常，"Time limit already exhausted"）、类型前置条件不满足（SecurityException）
            if (Build.VERSION.SDK_INT >= 31 && e is ForegroundServiceStartNotAllowedException) {
                Log.w(TAG, "startForeground not allowed: ${e.message}")
            } else {
                Log.w(TAG, "startForeground failed: $e")
            }
            false
        }
    }

    private fun refreshForeground(s: ServiceController.State) {
        val types = FgsPolicy.types(s.keepAlive, Build.VERSION.SDK_INT)
        if (types != currentTypes) {
            // 类型变化（开/关常驻）：重新 startForeground 换类型；失败就保持原类型
            goForeground(s)
        } else {
            Notify.post(this, null, NOTIFICATION_ID, buildNotification(s))
        }
    }

    private fun scheduleIdleCheck() {
        idleJob?.cancel()
        idleJob = scope.launch {
            while (true) {
                val s = ServiceController.state.value
                val now = SystemClock.elapsedRealtime()
                if (FgsPolicy.shouldRun(s, now)) {
                    if (s.keepAlive || s.active > 0 || s.sending > 0) return@launch   // 状态变化时会再调度
                    delay((s.graceUntilMs - now).coerceAtLeast(1000))
                    continue
                }
                delay(IDLE_STOP_DELAY_MS)
                if (!FgsPolicy.shouldRun(ServiceController.state.value, SystemClock.elapsedRealtime())) {
                    Log.i(TAG, "idle, stopping")
                    stopForegroundCompat()
                    stopSelf()
                }
                return@launch
            }
        }
    }

    private fun handleSend(intent: Intent) {
        val peers = intent.getStringArrayListExtra(EXTRA_PEERS).orEmpty().filter { it.isNotBlank() }
        val text = intent.getStringExtra(EXTRA_TEXT)
        val uris = buildList { intent.clipData?.let { c -> for (i in 0 until c.itemCount) c.getItemAt(i).uri?.let { add(it) } } }
        if (peers.isEmpty() || (uris.isEmpty() && text.isNullOrBlank())) return
        ServiceController.beginSend()
        scope.launch {
            try {
                val repo = ServiceLocator.repo
                repo.peers.value.filter { it.id in peers }.forEach { ShortcutPublisher.push(this@LancatService, it) }
                if (!text.isNullOrBlank()) repo.sendText(peers, text)
                if (uris.isNotEmpty()) {
                    val staged = withContext(Dispatchers.IO) { ShareStaging.stage(this@LancatService, uris) }
                    repo.sendFiles(peers, staged.map { it.uri.toString() })
                }
            } catch (e: Exception) {
                Log.w(TAG, "send: $e")
            } finally {
                ServiceController.endSend()
            }
        }
    }

    /** Android 15+：dataSync / mediaProcessing 用完 6 小时额度 */
    override fun onTimeout(startId: Int, fgsType: Int) {
        Log.w(TAG, "onTimeout type=$fgsType")
        onTimeoutCommon()
    }

    /** Android 14 只对 shortService 回调；我们不用 shortService，保险起见同样处理 */
    @Deprecated("API 35 起由 onTimeout(int, int) 取代")
    override fun onTimeout(startId: Int) {
        onTimeoutCommon()
    }

    private fun onTimeoutCommon() {
        val n = NotificationCompat.Builder(this, Channels.XFER)
            .setSmallIcon(R.drawable.ic_notify)
            .setColor(0xFFD97757.toInt())
            .setContentTitle(getString(R.string.svc_timeout_title))
            .setContentText(getString(R.string.svc_timeout_text))
            .setContentIntent(openApp())
            .setAutoCancel(true)
            .build()
        Notify.post(this, "svc-timeout", 3, n)
        stopForegroundCompat()
        stopSelf()
    }

    private fun stopForegroundCompat() {
        foreground = false
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
    }

    override fun onDestroy() {
        running = false
        foreground = false
        NetPresence.release("service")
        scope.cancel()
        super.onDestroy()
    }

    private fun openApp(): PendingIntent = PendingIntent.getActivity(
        this, 1,
        Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

    private fun buildNotification(s: ServiceController.State): Notification {
        val b = NotificationCompat.Builder(this, Channels.SVC)
            .setSmallIcon(R.drawable.ic_notify)
            .setColor(0xFFD97757.toInt())
            .setOngoing(true)
            .setSilent(true)
            .setShowWhen(false)
            .setPriority(NotificationCompat.PRIORITY_MIN)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setContentIntent(openApp())
            // Android 12+ 前台服务通知默认可能延迟 10 秒显示；传输时立刻显示
            .setForegroundServiceBehavior(
                if (s.active > 0) NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE else NotificationCompat.FOREGROUND_SERVICE_DEFAULT
            )
        if (s.keepAlive) {
            val stop = PendingIntent.getService(
                this, 2,
                Intent(this, LancatService::class.java).setAction(ACTION_STOP_KEEPALIVE),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            b.setContentTitle(getString(R.string.svc_keepalive_title))
                .setContentText(if (s.active > 0) "正在传输 ${s.active} 个文件" else getString(R.string.svc_keepalive_text))
                .addAction(0, getString(R.string.svc_action_stop_keepalive), stop)
        } else {
            b.setContentTitle(getString(R.string.svc_transfer_title))
                .setContentText(if (s.active > 0) "${s.active} 个文件进行中" else "正在准备…")
        }
        return b.build()
    }

    companion object {
        private const val TAG = "lancat-svc"
        const val NOTIFICATION_ID = 1001
        private const val IDLE_STOP_DELAY_MS = 3000L
        const val ACTION_ENSURE = "fan.lancat.service.ENSURE"
        const val ACTION_SEND = "fan.lancat.service.SEND"
        const val ACTION_STOP_KEEPALIVE = "fan.lancat.service.STOP_KEEPALIVE"
        const val EXTRA_PEERS = "fan.lancat.extra.PEERS"
        const val EXTRA_TEXT = "fan.lancat.extra.TEXT"

        @Volatile var running = false
            private set

        /** 确保服务在跑。返回 false 表示系统不允许（后台启动限制等），调用方降级。 */
        fun ensure(ctx: Context): Boolean = try {
            ContextCompat.startForegroundService(ctx, Intent(ctx, LancatService::class.java).setAction(ACTION_ENSURE))
            true
        } catch (e: Exception) {
            Log.w(TAG, "ensure: $e")
            false
        }

        /** 构造发送 Intent：uri 放进 ClipData 并带读授权，授权随之转给服务 */
        fun sendIntent(ctx: Context, peerIds: List<String>, uris: List<Uri>, text: String?): Intent {
            val i = Intent(ctx, LancatService::class.java)
                .setAction(ACTION_SEND)
                .putStringArrayListExtra(EXTRA_PEERS, ArrayList(peerIds))
            text?.let { i.putExtra(EXTRA_TEXT, it) }
            if (uris.isNotEmpty()) {
                val clip = ClipData("lancat-share", arrayOf("*/*"), ClipData.Item(uris[0]))
                for (k in 1 until uris.size) clip.addItem(ClipData.Item(uris[k]))
                i.clipData = clip
                i.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            return i
        }

        /**
         * 发送（分享入口用）。调用方必须处于前台（ShareActivity 可见），否则系统不让起前台服务；
         * 万一起不来就退化为直接交给 core（此时授权只在 ShareActivity 存活期间有效，小文件仍会先复制）。
         */
        fun send(ctx: Context, peerIds: List<String>, uris: List<Uri>, text: String?) {
            val intent = sendIntent(ctx, peerIds, uris, text)
            try {
                ContextCompat.startForegroundService(ctx, intent)
            } catch (e: Exception) {
                Log.w(TAG, "send via service failed, fallback direct: $e")
                val app = ctx.applicationContext
                ServiceLocator.appScope.launch {
                    val repo = ServiceLocator.repo
                    if (!text.isNullOrBlank()) runCatching { repo.sendText(peerIds, text) }
                    if (uris.isNotEmpty()) {
                        val staged = withContext(Dispatchers.IO) { ShareStaging.stage(app, uris) }
                        runCatching { repo.sendFiles(peerIds, staged.map { it.uri.toString() }) }
                    }
                }
            }
        }
    }
}
