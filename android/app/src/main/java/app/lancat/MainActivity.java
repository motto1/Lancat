package app.lancat;

import android.Manifest;
import android.app.NativeActivity;
import android.content.Intent;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.view.View;
import android.view.Window;
import android.view.WindowManager;

import java.util.ArrayList;

/**
 * Slint 的 android-activity 后端跑在 NativeActivity 上（liblancat.so 的 android_main）。
 * 这里只加：铺满全屏、系统分享入口、文件选择结果、运行时权限、返回键交给 Rust。
 */
public class MainActivity extends NativeActivity {
    static final String ACTION_OPEN = "app.lancat.OPEN";
    static final int REQ_PICK = 41, REQ_PERM = 42;
    private int pickMode = -1;
    boolean askedNotif;
    private static boolean askedOnce;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        Bridge.activity = this;
        edgeToEdge();
        handleIntent(getIntent());
        Bridge.startService();
        askPermissions();
    }

    @SuppressWarnings("deprecation")
    private void edgeToEdge() {
        Window w = getWindow();
        if (Build.VERSION.SDK_INT >= 30) {
            w.setDecorFitsSystemWindows(false);
        } else {
            w.getDecorView().setSystemUiVisibility(View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                    | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
        }
        w.setStatusBarColor(0);
        w.setNavigationBarColor(0);
        if (Build.VERSION.SDK_INT >= 29) {
            w.setNavigationBarContrastEnforced(false);
        }
        if (Build.VERSION.SDK_INT >= 28) {
            WindowManager.LayoutParams lp = w.getAttributes();
            lp.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            w.setAttributes(lp);
        }
    }

    private void askPermissions() {
        if (askedOnce) return;
        askedOnce = true;
        ArrayList<String> p = new ArrayList<>();
        if (Build.VERSION.SDK_INT >= 33 && !Bridge.granted(Manifest.permission.POST_NOTIFICATIONS)) {
            p.add(Manifest.permission.POST_NOTIFICATIONS);
            askedNotif = true;
        }
        if (Build.VERSION.SDK_INT <= 29 && !Bridge.granted(Manifest.permission.WRITE_EXTERNAL_STORAGE)) {
            p.add(Manifest.permission.WRITE_EXTERNAL_STORAGE);
        }
        if (!p.isEmpty()) requestPermissions(p.toArray(new String[0]), REQ_PERM);
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] perms, int[] res) {
        super.onRequestPermissionsResult(code, perms, res);
        // Rust 重新看接收目录（拿到存储权限后换到 Download/Lancat）并重新诊断
        Bridge.post("resume");
    }

    @Override
    protected void onNewIntent(Intent i) {
        super.onNewIntent(i);
        setIntent(i);
        handleIntent(i);
    }

    @SuppressWarnings("deprecation")
    private void handleIntent(Intent i) {
        if (i == null || i.getBooleanExtra("lancat_handled", false)) return;
        i.putExtra("lancat_handled", true);
        String a = i.getAction();
        if (ACTION_OPEN.equals(a)) {
            String peer = i.getStringExtra("peer");
            if (peer != null) {
                Bridge.clearMessages(peer);
                Bridge.post("open", peer);
            }
            return;
        }
        ArrayList<Uri> uris = new ArrayList<>();
        if (Intent.ACTION_SEND.equals(a)) {
            Uri u = i.getParcelableExtra(Intent.EXTRA_STREAM);
            if (u != null) uris.add(u);
        } else if (Intent.ACTION_SEND_MULTIPLE.equals(a)) {
            ArrayList<Uri> l = i.getParcelableArrayListExtra(Intent.EXTRA_STREAM);
            if (l != null) for (Uri u : l) if (u != null) uris.add(u);
        } else {
            return;
        }
        if (uris.isEmpty() && i.getClipData() != null) {
            for (int k = 0; k < i.getClipData().getItemCount(); k++) {
                Uri u = i.getClipData().getItemAt(k).getUri();
                if (u != null) uris.add(u);
            }
        }
        if (!uris.isEmpty()) {
            Bridge.shareUris(uris);
            return;
        }
        CharSequence t = i.getCharSequenceExtra(Intent.EXTRA_TEXT);
        if (t != null && t.length() > 0) Bridge.post("sharetext", t.toString());
    }

    /** Bridge.pickBlocking → 主线程：0 文件（多选）1 文件夹 2 贴纸文件 */
    boolean startPick(int mode) {
        try {
            Intent i;
            if (mode == 1) {
                i = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
            } else {
                i = new Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*")
                        .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            }
            pickMode = mode;
            startActivityForResult(i, REQ_PICK);
            return true;
        } catch (Exception e) {
            Bridge.toast("无法打开文件选择器");
            return false;
        }
    }

    @Override
    protected void onActivityResult(int req, int res, Intent data) {
        super.onActivityResult(req, res, data);
        if (req != REQ_PICK) return;
        int mode = pickMode;
        pickMode = -1;
        Bridge.onPicked(mode, res == RESULT_OK ? data : null);
    }

    @Override
    protected void onResume() {
        super.onResume();
        Bridge.activity = this;
        Bridge.foreground = true;
        Bridge.post("resume");
    }

    @Override
    protected void onPause() {
        Bridge.foreground = false;
        Bridge.post("pause");
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        if (Bridge.activity == this) Bridge.activity = null;
        Bridge.cancelPick();
        super.onDestroy();
    }

    /**
     * Slint 没处理的返回键会调 finish()（Android 14 起走 OnBackInvokedCallback），老系统走 onBackPressed → finish()。
     * 界面在跑时交给 Rust：设置页回面板、关弹层，否则退到后台（节点和前台服务继续）。
     */
    @Override
    public void finish() {
        if (Bridge.uiAlive()) {
            Bridge.post("back");
        } else {
            super.finish();
        }
    }
}
