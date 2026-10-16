package app.lancat;

import android.app.Application;

public class LancatApp extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        Bridge.init(this);
    }
}
