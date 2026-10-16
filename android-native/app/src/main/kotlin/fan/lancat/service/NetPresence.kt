package fan.lancat.service

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import android.util.Log
import fan.lancat.ServiceLocator
import kotlinx.coroutines.launch

/**
 * 「本机在局域网上可见」期间要做的事，按持有者引用计数（"ui" = MainActivity 可见，"service" = LancatService 在跑）：
 * - Wi-Fi MulticastLock：不拿的话很多机型在灭屏/省电时丢弃组播包，mDNS / UDP 广播发现失效
 * - ConnectivityManager.NetworkCallback（Wi-Fi + 以太网）：网络或 IP 变化时，去抖 1.5 秒后调用
 *   LancatCoreRepository.onNetworkChanged()（让 core 重绑套接字、重连）并 refreshPeers()。
 *   用「网络 + 地址」签名去重：注册时系统会立刻回调一次现有网络，签名没变就不打扰 core。
 */
object NetPresence {
    private const val TAG = "lancat-net"
    private const val DEBOUNCE_MS = 1500L

    private lateinit var app: Context
    private val holders = mutableSetOf<String>()
    private var mlock: WifiManager.MulticastLock? = null
    private var registered = false
    private val main = Handler(Looper.getMainLooper())
    private val links = mutableMapOf<Network, String>()
    @Volatile private var lastSignature: String? = null
    var notifyCount = 0
        private set

    fun init(ctx: Context) {
        app = ctx.applicationContext
    }

    @Synchronized
    fun acquire(holder: String) {
        if (!::app.isInitialized) return
        val wasEmpty = holders.isEmpty()
        holders += holder
        if (wasEmpty) up()
    }

    @Synchronized
    fun release(holder: String) {
        if (!holders.remove(holder)) return
        if (holders.isEmpty()) down()
    }

    fun isHeld(): Boolean = mlock?.isHeld == true

    private fun up() {
        try {
            val wm = app.getSystemService(Context.WIFI_SERVICE) as WifiManager
            val l = mlock ?: wm.createMulticastLock("lancat-discovery").apply { setReferenceCounted(false) }.also { mlock = it }
            if (!l.isHeld) l.acquire()
        } catch (e: Exception) {
            Log.w(TAG, "multicast lock: $e")
        }
        try {
            val cm = app.getSystemService(ConnectivityManager::class.java)
            val req = NetworkRequest.Builder()
                .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                .addTransportType(NetworkCapabilities.TRANSPORT_ETHERNET)
                .build()
            cm.registerNetworkCallback(req, callback)
            registered = true
        } catch (e: Exception) {
            Log.w(TAG, "registerNetworkCallback: $e")
        }
    }

    private fun down() {
        try { mlock?.takeIf { it.isHeld }?.release() } catch (_: Exception) {}
        if (registered) {
            try { app.getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(callback) } catch (_: Exception) {}
            registered = false
        }
        synchronized(links) { links.clear() }
    }

    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onLinkPropertiesChanged(network: Network, lp: LinkProperties) {
            val addrs = lp.linkAddresses.map { it.toString() }.sorted().joinToString(",")
            synchronized(links) { links[network] = "${lp.interfaceName}:$addrs" }
            schedule()
        }

        override fun onLost(network: Network) {
            synchronized(links) { links.remove(network) }
            schedule()
        }
    }

    private val fire = Runnable {
        val sig = synchronized(links) { links.values.sorted().joinToString("|") }
        val prev = lastSignature
        lastSignature = sig
        if (prev == null || prev == sig) return@Runnable   // 第一次只记基线；签名没变不通知
        notifyCount++
        Log.i(TAG, "network changed: $prev -> $sig")
        try {
            ServiceLocator.onNetworkChanged()
            ServiceLocator.appScope.launch { runCatching { ServiceLocator.repo.refreshPeers() } }
        } catch (e: Exception) {
            Log.w(TAG, "onNetworkChanged: $e")
        }
    }

    private fun schedule() {
        main.removeCallbacks(fire)
        main.postDelayed(fire, DEBOUNCE_MS)
    }

    /** 单测用：直接喂签名 */
    internal fun feedForTest(network: Network?, sig: String?) {
        synchronized(links) { if (network != null) { if (sig == null) links.remove(network) else links[network] = sig } }
        fire.run()
    }
}
