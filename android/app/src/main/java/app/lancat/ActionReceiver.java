package app.lancat;

import android.app.NotificationManager;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** 通知上的按钮：传输的暂停 / 继续 / 取消 / 重试交给 Rust（通知线程轮询 takeActions）；后台服务的「退出」 */
public class ActionReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context ctx, Intent i) {
        String a = i.getAction();
        if (a == null || !a.startsWith("app.lancat.")) return;
        a = a.substring("app.lancat.".length());
        String id = i.getStringExtra("id");
        if (a.equals("quit")) {
            ctx.stopService(new Intent(ctx, LancatService.class));
            MainActivity act = Bridge.activity;
            if (act != null) act.finishAndRemoveTask();
            // 节点在原生库里，没有单独的停止接口：整个进程退出（SQLite 是 WAL，随时退出安全）
            Bridge.main.postDelayed(() -> android.os.Process.killProcess(android.os.Process.myPid()), 300);
            return;
        }
        if (id == null) return;
        if (a.equals("retry")) {
            try {
                ctx.getSystemService(NotificationManager.class).cancel(Bridge.nid("d:" + id));
            } catch (Exception ignored) {
            }
        }
        Bridge.action(a, id);
    }
}
