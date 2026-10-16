package app.lancat;

import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;

import androidx.core.app.NotificationCompat;
import androidx.core.app.ServiceCompat;

/**
 * 前台服务（dataSync）：应用退到后台后进程不被回收，并持有
 * Wi-Fi 组播锁（mDNS / UDP 广播发现要靠它收组播包）和部分唤醒锁（屏幕关了也能继续收发文件）。
 * 节点本身在原生库里（android_main 启动），这里只负责让进程活着、锁拿着。
 */
public class LancatService extends Service {
    static volatile boolean running;
    private static WifiManager.MulticastLock mlock;
    private static PowerManager.WakeLock wlock;

    static boolean multicastHeld() {
        WifiManager.MulticastLock l = mlock;
        return l != null && l.isHeld();
    }

    @Override
    public void onCreate() {
        super.onCreate();
        Intent open = new Intent(this, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent tap = PendingIntent.getActivity(this, 1, open, PendingIntent.FLAG_IMMUTABLE);
        Intent quit = new Intent(this, ActionReceiver.class).setAction("app.lancat.quit");
        PendingIntent q = PendingIntent.getBroadcast(this, 2, quit, PendingIntent.FLAG_IMMUTABLE);
        NotificationCompat.Builder b = new NotificationCompat.Builder(this, Bridge.CH_SVC)
                .setSmallIcon(R.drawable.ic_stat)
                .setContentTitle("Lancat 正在后台运行")
                .setContentText("局域网里的设备能发现本机，随时可以接收文件")
                .setOngoing(true)
                .setSilent(true)
                .setPriority(NotificationCompat.PRIORITY_MIN)
                .setContentIntent(tap)
                .addAction(0, "退出 Lancat", q);
        try {
            ServiceCompat.startForeground(this, 1, b.build(), Build.VERSION.SDK_INT >= 29 ? ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC : 0);
        } catch (Exception e) {
            android.util.Log.w("lancat", "startForeground: " + e);
        }
        try {
            WifiManager wm = (WifiManager) getApplicationContext().getSystemService(WIFI_SERVICE);
            if (mlock == null) {
                mlock = wm.createMulticastLock("lancat-discovery");
                mlock.setReferenceCounted(false);
            }
            mlock.acquire();
        } catch (Exception e) {
            android.util.Log.w("lancat", "multicast lock: " + e);
        }
        try {
            PowerManager pm = (PowerManager) getSystemService(POWER_SERVICE);
            if (wlock == null) {
                wlock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "lancat:transfer");
                wlock.setReferenceCounted(false);
            }
            wlock.acquire();
        } catch (Exception e) {
            android.util.Log.w("lancat", "wake lock: " + e);
        }
        running = true;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // 进程被系统杀掉后不要只把服务拉起来：节点在 Activity 启动时才起
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        running = false;
        try {
            if (mlock != null && mlock.isHeld()) mlock.release();
        } catch (Exception ignored) {
        }
        try {
            if (wlock != null && wlock.isHeld()) wlock.release();
        } catch (Exception ignored) {
        }
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
