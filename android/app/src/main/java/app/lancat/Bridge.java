package app.lancat;

import android.Manifest;
import android.app.Activity;
import android.app.DownloadManager;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.ActivityNotFoundException;
import android.content.ContentResolver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.res.Configuration;
import android.database.Cursor;
import android.media.MediaScannerConnection;
import android.media.Ringtone;
import android.media.RingtoneManager;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.os.Handler;
import android.os.Looper;
import android.os.PowerManager;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;
import android.provider.Settings;
import android.view.View;
import android.view.Window;
import android.view.WindowInsetsController;
import android.webkit.MimeTypeMap;
import android.widget.Toast;

import androidx.core.app.NotificationCompat;
import androidx.core.app.NotificationManagerCompat;
import androidx.core.content.FileProvider;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Rust（src/android/jvm.rs）调用的静态方法。方向只有 Rust → Java：Java 这边的事件（分享进来的文件、
 * 通知按钮、返回键、前后台切换）放进队列，Rust 定时取走（poll / takeActions）。
 * 这些方法会在任意原生线程上被调用：碰界面的操作都转到主线程。
 */
public final class Bridge {
    static Context app;
    static volatile MainActivity activity;
    static volatile boolean foreground;
    /** Rust 界面线程最后一次来取事件的时间（判断界面是否在运行） */
    static volatile long lastPoll;

    static final Handler main = new Handler(Looper.getMainLooper());
    /** 复制分享 / 选择的内容（可能很大）：一个后台线程，按顺序做 */
    static final ExecutorService io = Executors.newSingleThreadExecutor();

    private static final ArrayList<String> events = new ArrayList<>();
    private static final ArrayList<String> actions = new ArrayList<>();
    private static final HashMap<String, ArrayList<String>> msgLines = new HashMap<>();

    static final String CH_MSG = "msg", CH_XFER = "xfer", CH_DONE = "done", CH_SVC = "svc";

    private Bridge() {}

    static void init(Context ctx) {
        app = ctx.getApplicationContext();
        if (Build.VERSION.SDK_INT >= 26) {
            NotificationManager nm = app.getSystemService(NotificationManager.class);
            NotificationChannel m = new NotificationChannel(CH_MSG, "新消息", NotificationManager.IMPORTANCE_HIGH);
            NotificationChannel x = new NotificationChannel(CH_XFER, "传输进度", NotificationManager.IMPORTANCE_LOW);
            x.setShowBadge(false);
            NotificationChannel d = new NotificationChannel(CH_DONE, "传输完成", NotificationManager.IMPORTANCE_DEFAULT);
            NotificationChannel s = new NotificationChannel(CH_SVC, "后台运行", NotificationManager.IMPORTANCE_MIN);
            s.setShowBadge(false);
            nm.createNotificationChannel(m);
            nm.createNotificationChannel(x);
            nm.createNotificationChannel(d);
            nm.createNotificationChannel(s);
        }
        // 分享 / 选择时复制进缓存的文件：发送失败后要靠它们断点重试，保留 7 天
        io.execute(() -> prune(new File(app.getCacheDir(), "share"), 7L * 24 * 3600 * 1000));
    }

    // ------------------------------------------------------------------ 事件队列

    static String esc(String s) {
        if (s == null) return "";
        StringBuilder b = new StringBuilder(s.length() + 8);
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '\\') b.append("\\\\");
            else if (c == '\n') b.append("\\n");
            else if (c == '\t') b.append("\\t");
            else if (c != '\r') b.append(c);
        }
        return b.toString();
    }

    static String line(String... f) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < f.length; i++) {
            if (i > 0) b.append('\t');
            b.append(esc(f[i]));
        }
        return b.toString();
    }

    static void post(String... fields) {
        synchronized (events) {
            events.add(line(fields));
        }
    }

    static void post(String kind, List<String> args) {
        ArrayList<String> f = new ArrayList<>();
        f.add(kind);
        f.addAll(args);
        post(f.toArray(new String[0]));
    }

    public static String poll() {
        lastPoll = System.currentTimeMillis();
        synchronized (events) {
            if (events.isEmpty()) return "";
            String s = String.join("\n", events);
            events.clear();
            return s;
        }
    }

    static void action(String act, String id) {
        synchronized (actions) {
            actions.add(line(act, id));
        }
    }

    public static String takeActions() {
        synchronized (actions) {
            if (actions.isEmpty()) return "";
            String s = String.join("\n", actions);
            actions.clear();
            return s;
        }
    }

    static boolean uiAlive() {
        return System.currentTimeMillis() - lastPoll < 3000;
    }

    // ------------------------------------------------------------------ 系统信息

    public static String filesDir() {
        return app.getFilesDir().getAbsolutePath();
    }

    public static String deviceName() {
        try {
            String n = Settings.Global.getString(app.getContentResolver(), "device_name");
            if (n != null && !n.trim().isEmpty()) return n.trim();
        } catch (Exception ignored) {
        }
        String m = Build.MODEL == null ? "" : Build.MODEL;
        String b = Build.MANUFACTURER == null ? "" : Build.MANUFACTURER;
        if (b.isEmpty() || m.toLowerCase(Locale.ROOT).startsWith(b.toLowerCase(Locale.ROOT))) return m.isEmpty() ? "Android" : m;
        return b.substring(0, 1).toUpperCase(Locale.ROOT) + b.substring(1) + " " + m;
    }

    static boolean granted(String perm) {
        return app.checkSelfPermission(perm) == PackageManager.PERMISSION_GRANTED;
    }

    /** 收到的文件放 Download/Lancat；Android 8–10 没给存储权限时退到应用自己的目录 */
    public static String receiveDir() {
        File pub = new File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "Lancat");
        boolean can = Build.VERSION.SDK_INT >= 30 || granted(Manifest.permission.WRITE_EXTERNAL_STORAGE);
        if (can && writable(pub)) return pub.getAbsolutePath();
        File f = app.getExternalFilesDir("Lancat");
        if (f == null) f = new File(app.getFilesDir(), "Received");
        f.mkdirs();
        return f.getAbsolutePath();
    }

    static boolean writable(File d) {
        try {
            if (!d.isDirectory() && !d.mkdirs()) return false;
            File t = new File(d, ".lancat-probe");
            new FileOutputStream(t).close();
            t.delete();
            return true;
        } catch (Exception e) {
            return false;
        }
    }

    public static boolean isNightMode() {
        int m = app.getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        return m == Configuration.UI_MODE_NIGHT_YES;
    }

    public static boolean isForeground() {
        return foreground;
    }

    public static void moveToBack() {
        final Activity a = activity;
        if (a != null) main.post(() -> a.moveTaskToBack(true));
    }

    public static void startService() {
        main.post(() -> {
            try {
                androidx.core.content.ContextCompat.startForegroundService(app, new Intent(app, LancatService.class));
            } catch (Exception e) {
                android.util.Log.w("lancat", "start service: " + e);
            }
        });
    }

    public static void playChime() {
        try {
            Ringtone r = RingtoneManager.getRingtone(app, RingtoneManager.getDefaultUri(RingtoneManager.TYPE_NOTIFICATION));
            if (r != null) r.play();
        } catch (Exception ignored) {
        }
    }

    @SuppressWarnings("deprecation")
    public static void setLightBars(final boolean light) {
        final Activity a = activity;
        if (a == null) return;
        main.post(() -> {
            Window w = a.getWindow();
            if (Build.VERSION.SDK_INT >= 30) {
                WindowInsetsController c = w.getInsetsController();
                if (c != null) {
                    int mask = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                    c.setSystemBarsAppearance(light ? mask : 0, mask);
                }
            } else {
                View v = w.getDecorView();
                int f = v.getSystemUiVisibility();
                int mask = View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
                v.setSystemUiVisibility(light ? (f | mask) : (f & ~mask));
            }
        });
    }

    public static String diagInfo() {
        StringBuilder b = new StringBuilder();
        String net = "none";
        try {
            ConnectivityManager cm = app.getSystemService(ConnectivityManager.class);
            boolean wifi = false, eth = false, cell = false;
            for (Network n : cm.getAllNetworks()) {
                NetworkCapabilities c = cm.getNetworkCapabilities(n);
                if (c == null) continue;
                wifi |= c.hasTransport(NetworkCapabilities.TRANSPORT_WIFI);
                eth |= c.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET);
                cell |= c.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR);
            }
            net = wifi ? "wifi" : eth ? "ethernet" : cell ? "cellular" : "none";
        } catch (Exception ignored) {
        }
        b.append("net=").append(net).append('\n');
        b.append("service=").append(LancatService.running ? 1 : 0).append('\n');
        b.append("mlock=").append(LancatService.multicastHeld() ? 1 : 0).append('\n');
        b.append("notif=").append(NotificationManagerCompat.from(app).areNotificationsEnabled() ? 1 : 0).append('\n');
        boolean bat = false;
        try {
            PowerManager pm = app.getSystemService(PowerManager.class);
            bat = pm.isIgnoringBatteryOptimizations(app.getPackageName());
        } catch (Exception ignored) {
        }
        b.append("battery=").append(bat ? 1 : 0).append('\n');
        return b.toString();
    }

    /** 诊断里的修复按钮 */
    public static void fix(final String id) {
        main.post(() -> {
            String pkg = app.getPackageName();
            Intent i = null;
            switch (id) {
                case "service":
                    startService();
                    return;
                case "wifi":
                    i = new Intent(Settings.ACTION_WIFI_SETTINGS);
                    break;
                case "notif": {
                    MainActivity a = activity;
                    if (Build.VERSION.SDK_INT >= 33 && a != null && !granted(Manifest.permission.POST_NOTIFICATIONS)
                            && !a.askedNotif) {
                        a.askedNotif = true;
                        a.requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, MainActivity.REQ_PERM);
                        return;
                    }
                    i = new Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, pkg);
                    break;
                }
                case "battery":
                    i = new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:" + pkg));
                    break;
                default:
                    return;
            }
            start(i);
        });
    }

    static void start(Intent i) {
        Activity a = activity;
        try {
            if (a != null) {
                a.startActivity(i);
            } else {
                app.startActivity(i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            }
        } catch (ActivityNotFoundException e) {
            toast("系统里没有对应的设置页面");
        } catch (Exception e) {
            android.util.Log.w("lancat", "start " + i + ": " + e);
        }
    }

    static void toast(final String s) {
        main.post(() -> Toast.makeText(app, s, Toast.LENGTH_SHORT).show());
    }

    // ------------------------------------------------------------------ 文件

    static Intent viewIntent(String path) {
        File f = new File(path);
        if (f.isDirectory()) {
            return new Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        Uri uri;
        try {
            uri = FileProvider.getUriForFile(app, "app.lancat.files", f);
        } catch (Exception e) {
            return new Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        String ext = MimeTypeMap.getFileExtensionFromUrl(Uri.fromFile(f).toString());
        String mime = ext == null ? null : MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext.toLowerCase(Locale.ROOT));
        if (mime == null) mime = "*/*";
        Intent v = new Intent(Intent.ACTION_VIEW).setDataAndType(uri, mime).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        return Intent.createChooser(v, "打开 " + f.getName()).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_GRANT_READ_URI_PERMISSION);
    }

    public static void openFile(final String path) {
        main.post(() -> {
            if (!new File(path).exists()) {
                toast("文件已不存在");
                return;
            }
            start(viewIntent(path));
        });
    }

    /** 收完的文件让相册 / 文件管理马上看到 */
    public static void scanFile(String path) {
        ArrayList<String> all = new ArrayList<>();
        walk(new File(path), all);
        if (all.isEmpty()) return;
        try {
            MediaScannerConnection.scanFile(app, all.toArray(new String[0]), null, null);
        } catch (Exception ignored) {
        }
    }

    static void walk(File f, List<String> out) {
        if (f.isDirectory()) {
            File[] c = f.listFiles();
            if (c != null) for (File x : c) walk(x, out);
        } else if (f.isFile()) {
            out.add(f.getAbsolutePath());
        }
    }

    static void prune(File dir, long maxAge) {
        File[] c = dir.listFiles();
        if (c == null) return;
        long now = System.currentTimeMillis();
        for (File x : c) {
            if (now - x.lastModified() > maxAge) deleteTree(x);
        }
    }

    static void deleteTree(File f) {
        File[] c = f.listFiles();
        if (c != null) for (File x : c) deleteTree(x);
        f.delete();
    }

    static File freshDir() {
        File d = new File(new File(app.getCacheDir(), "share"), Long.toString(System.nanoTime()));
        d.mkdirs();
        return d;
    }

    static String safeName(String n) {
        if (n == null || n.trim().isEmpty()) n = "file";
        n = n.replace('/', '_').replace('\\', '_').replace('\0', '_').trim();
        if (n.equals(".") || n.equals("..")) n = "file";
        return n;
    }

    static File uniq(File dir, String name) {
        File f = new File(dir, name);
        if (!f.exists()) return f;
        int dot = name.lastIndexOf('.');
        String base = dot > 0 ? name.substring(0, dot) : name, ext = dot > 0 ? name.substring(dot) : "";
        for (int i = 2; ; i++) {
            f = new File(dir, base + " (" + i + ")" + ext);
            if (!f.exists()) return f;
        }
    }

    static String queryName(ContentResolver cr, Uri u, String col) {
        try (Cursor c = cr.query(u, new String[]{col}, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0)) return c.getString(0);
        } catch (Exception ignored) {
        }
        return null;
    }

    static void copy(ContentResolver cr, Uri u, File to) throws Exception {
        try (InputStream in = cr.openInputStream(u); OutputStream out = new FileOutputStream(to)) {
            if (in == null) throw new Exception("无法读取 " + u);
            byte[] buf = new byte[256 * 1024];
            int n;
            while ((n = in.read(buf)) > 0) out.write(buf, 0, n);
        }
    }

    /** content:// → 缓存里的本地文件（Lancat 的发送按路径读文件、断点续传也要能重新打开） */
    static String copyUri(Uri u, File dir) {
        ContentResolver cr = app.getContentResolver();
        try {
            if ("file".equals(u.getScheme()) && u.getPath() != null && new File(u.getPath()).canRead()) {
                return u.getPath();
            }
            String name = queryName(cr, u, OpenableColumns.DISPLAY_NAME);
            if (name == null) name = u.getLastPathSegment();
            File to = uniq(dir, safeName(name));
            copy(cr, u, to);
            return to.getAbsolutePath();
        } catch (Exception e) {
            android.util.Log.w("lancat", "copy " + u + ": " + e);
            return null;
        }
    }

    static String copyTree(Uri tree, File dir) {
        ContentResolver cr = app.getContentResolver();
        try {
            String rootId = DocumentsContract.getTreeDocumentId(tree);
            Uri rootDoc = DocumentsContract.buildDocumentUriUsingTree(tree, rootId);
            String name = queryName(cr, rootDoc, DocumentsContract.Document.COLUMN_DISPLAY_NAME);
            File root = uniq(dir, safeName(name == null ? "folder" : name));
            root.mkdirs();
            copyChildren(cr, tree, rootId, root);
            return root.getAbsolutePath();
        } catch (Exception e) {
            android.util.Log.w("lancat", "copy tree " + tree + ": " + e);
            return null;
        }
    }

    static void copyChildren(ContentResolver cr, Uri tree, String parent, File into) throws Exception {
        Uri kids = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parent);
        String[] cols = {DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE};
        try (Cursor c = cr.query(kids, cols, null, null, null)) {
            if (c == null) return;
            while (c.moveToNext()) {
                String id = c.getString(0), name = safeName(c.getString(1)), mime = c.getString(2);
                if (DocumentsContract.Document.MIME_TYPE_DIR.equals(mime)) {
                    File d = uniq(into, name);
                    d.mkdirs();
                    copyChildren(cr, tree, id, d);
                } else {
                    copy(cr, DocumentsContract.buildDocumentUriUsingTree(tree, id), uniq(into, name));
                }
            }
        }
    }

    /** 系统分享进来的内容：后台复制完再交给 Rust（"share" 事件） */
    static void shareUris(final List<Uri> uris) {
        post("copying", Integer.toString(uris.size()));
        io.execute(() -> {
            File dir = freshDir();
            ArrayList<String> paths = new ArrayList<>();
            for (Uri u : uris) {
                String p = copyUri(u, dir);
                if (p != null) paths.add(p);
            }
            if (paths.isEmpty()) toast("读取分享的文件失败");
            post("share", paths);
        });
    }

    // ------------------------------------------------------------------ 选择文件（附件 / 文件夹 / 贴纸）

    private static final Object pickLock = new Object();
    private static AtomicReference<String> pickResult;
    private static java.util.concurrent.CountDownLatch pickDone;

    /** 在 Rust 的文件选择线程上调用：打开系统选择器并等到用户选完、内容复制好（最多 15 分钟） */
    public static String pickBlocking(final int mode) {
        final MainActivity a = activity;
        if (a == null) return "";
        final AtomicReference<String> res = new AtomicReference<>("");
        final java.util.concurrent.CountDownLatch latch = new java.util.concurrent.CountDownLatch(1);
        synchronized (pickLock) {
            if (pickDone != null) return "";
            pickResult = res;
            pickDone = latch;
        }
        main.post(() -> {
            if (!a.startPick(mode)) finishPick("");
        });
        try {
            latch.await(15, TimeUnit.MINUTES);
        } catch (InterruptedException ignored) {
        }
        synchronized (pickLock) {
            if (pickDone == latch) {
                pickDone = null;
                pickResult = null;
            }
        }
        return res.get();
    }

    static void finishPick(String r) {
        synchronized (pickLock) {
            if (pickDone == null) return;
            pickResult.set(r);
            pickDone.countDown();
            pickDone = null;
            pickResult = null;
        }
    }

    /** MainActivity.onActivityResult：mode 1 是文件夹（tree），其余是文件（可多选） */
    static void onPicked(final int mode, final Intent data) {
        if (data == null) {
            finishPick("");
            return;
        }
        final ArrayList<Uri> uris = new ArrayList<>();
        if (data.getClipData() != null) {
            for (int i = 0; i < data.getClipData().getItemCount(); i++) uris.add(data.getClipData().getItemAt(i).getUri());
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        if (uris.isEmpty()) {
            finishPick("");
            return;
        }
        toast(mode == 1 ? "正在读取文件夹…" : "正在读取 " + uris.size() + " 个文件…");
        io.execute(() -> {
            File dir = freshDir();
            StringBuilder b = new StringBuilder();
            for (Uri u : uris) {
                String p = mode == 1 ? copyTree(u, dir) : copyUri(u, dir);
                if (p != null) {
                    if (b.length() > 0) b.append('\n');
                    b.append(esc(p));
                }
            }
            finishPick(b.toString());
        });
    }

    static void cancelPick() {
        finishPick("");
    }

    // ------------------------------------------------------------------ 通知

    static int nid(String key) {
        return (key.hashCode() & 0x3fffffff) | 0x100;
    }

    static PendingIntent openPeer(String peer) {
        Intent i = new Intent(app, MainActivity.class).setAction(MainActivity.ACTION_OPEN).putExtra("peer", peer)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return PendingIntent.getActivity(app, nid("open:" + peer), i, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    static PendingIntent actionIntent(String act, String id) {
        Intent i = new Intent(app, ActionReceiver.class).setAction("app.lancat." + act).putExtra("id", id);
        return PendingIntent.getBroadcast(app, nid(act + ":" + id), i, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    static void addActions(NotificationCompat.Builder b, String id, String actions) {
        if (actions == null || actions.isEmpty()) return;
        for (String a : actions.split(",")) {
            String label;
            switch (a) {
                case "pause": label = "暂停"; break;
                case "resume": label = "继续"; break;
                case "cancel": label = "取消"; break;
                case "retry": label = "重试"; break;
                default: continue;
            }
            b.addAction(0, label, actionIntent(a, id));
        }
    }

    static void show(int id, NotificationCompat.Builder b) {
        try {
            NotificationManager nm = app.getSystemService(NotificationManager.class);
            nm.notify(id, b.build());
        } catch (Exception e) {
            android.util.Log.w("lancat", "notify: " + e);
        }
    }

    public static void notifyMessage(String peer, String title, String text) {
        ArrayList<String> ls;
        synchronized (msgLines) {
            ls = msgLines.get(peer);
            if (ls == null) msgLines.put(peer, ls = new ArrayList<>());
            ls.add(text);
            while (ls.size() > 6) ls.remove(0);
            ls = new ArrayList<>(ls);
        }
        NotificationCompat.InboxStyle st = new NotificationCompat.InboxStyle();
        for (String l : ls) st.addLine(l);
        NotificationCompat.Builder b = new NotificationCompat.Builder(app, CH_MSG)
                .setSmallIcon(R.drawable.ic_stat)
                .setContentTitle(title)
                .setContentText(text)
                .setStyle(ls.size() > 1 ? st : new NotificationCompat.BigTextStyle().bigText(text))
                .setNumber(ls.size())
                .setCategory(NotificationCompat.CATEGORY_MESSAGE)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .setAutoCancel(true)
                .setContentIntent(openPeer(peer));
        show(nid("m:" + peer), b);
    }

    static void clearMessages(String peer) {
        synchronized (msgLines) {
            msgLines.remove(peer);
        }
        try {
            app.getSystemService(NotificationManager.class).cancel(nid("m:" + peer));
        } catch (Exception ignored) {
        }
    }

    public static void notifyProgress(String id, String peer, String title, String text, int pct, String actions) {
        NotificationCompat.Builder b = new NotificationCompat.Builder(app, CH_XFER)
                .setSmallIcon(android.R.drawable.stat_sys_download)
                .setContentTitle(title)
                .setContentText(text)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setSilent(true)
                .setCategory(NotificationCompat.CATEGORY_PROGRESS)
                .setProgress(100, Math.max(0, Math.min(100, pct)), pct < 0)
                .setContentIntent(openPeer(peer));
        addActions(b, id, actions);
        show(nid("x:" + id), b);
    }

    public static void notifyDone(String id, String peer, String title, String text, String path, boolean ok, String actions) {
        PendingIntent tap = openPeer(peer);
        if (ok && path != null && !path.isEmpty()) {
            tap = PendingIntent.getActivity(app, nid("v:" + id), viewIntent(path), PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        }
        NotificationCompat.Builder b = new NotificationCompat.Builder(app, CH_DONE)
                .setSmallIcon(ok ? R.drawable.ic_stat : android.R.drawable.stat_notify_error)
                .setContentTitle(title)
                .setContentText(text)
                .setStyle(new NotificationCompat.BigTextStyle().bigText(text))
                .setAutoCancel(true)
                .setOnlyAlertOnce(true)
                .setContentIntent(tap);
        addActions(b, id, actions);
        try {
            app.getSystemService(NotificationManager.class).cancel(nid("x:" + id));
        } catch (Exception ignored) {
        }
        show(nid("d:" + id), b);
    }

    public static void cancelNotify(String id) {
        try {
            NotificationManager nm = app.getSystemService(NotificationManager.class);
            nm.cancel(nid("x:" + id));
            nm.cancel(nid("d:" + id));
        } catch (Exception ignored) {
        }
    }
}
