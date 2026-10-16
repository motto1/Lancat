package fan.lancat

import android.app.Application
import android.os.StrictMode
import fan.lancat.notify.Channels
import fan.lancat.notify.MessageNotifier
import fan.lancat.notify.TransferNotifier
import fan.lancat.service.NetPresence
import fan.lancat.service.ServiceController
import fan.lancat.share.ShareStaging
import fan.lancat.shortcut.ShortcutPublisher
import fan.lancat.tile.ReceiveTileService
import fan.lancat.update.Updater
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

class LancatApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        if (BuildConfig.DEBUG) {
            StrictMode.setVmPolicy(StrictMode.VmPolicy.Builder().detectLeakedClosableObjects().detectFileUriExposure().penaltyLog().build())
        }
        ServiceLocator.init(this)
        AppForeground.install(this)
        Channels.ensure(this)
        Updater.init(this)
        NetPresence.init(this)
        startRuntime()
    }

    /** 进程级收集器：设置镜像、通知、快捷方式、前台服务调度。全部挂在 ServiceLocator.appScope 上。 */
    private fun startRuntime() {
        val scope = ServiceLocator.appScope
        val repo = ServiceLocator.repo

        scope.launch {
            repo.settings.map { it.keepAlive }.distinctUntilChanged().collect { on ->
                SysPrefs.setKeepAlive(this@LancatApplication, on)
                ReceiveTileService.requestRefresh(this@LancatApplication)
            }
        }
        MessageNotifier.start(this, repo, scope)
        TransferNotifier.start(this, repo, scope)
        ShortcutPublisher.start(this, repo, scope)
        ServiceController.start(this, repo, scope)
        scope.launch(Dispatchers.IO) { ShareStaging.prune(this@LancatApplication) }
    }
}
