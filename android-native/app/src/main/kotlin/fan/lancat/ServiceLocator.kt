package fan.lancat

import android.content.Context
import fan.lancat.model.LancatRepository
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob

/**
 * 手写的服务定位器（不引 Hilt）：整个进程只有一个 LancatRepository。
 * Application.onCreate 里 init；Service / Receiver / Tile 在 Application 之后才会被创建，直接取 repo 即可。
 * 测试可以先调 [overrideForTest] 换成假实现。
 */
object ServiceLocator {
    @Volatile private var repoRef: LancatRepository? = null
    private lateinit var appContext: Context

    /** 进程级协程作用域：通知、快捷方式、服务调度这些常驻收集器都挂在这里 */
    val appScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    fun init(context: Context) {
        appContext = context.applicationContext
    }

    val context: Context get() = appContext

    val repo: LancatRepository
        get() = repoRef ?: synchronized(this) {
            repoRef ?: Bridge.createRepository(appContext).also { repoRef = it }
        }

    fun onNetworkChanged() = Bridge.onNetworkChanged(repo)

    fun overrideForTest(repo: LancatRepository?) {
        repoRef = repo
    }
}
