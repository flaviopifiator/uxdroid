package dev.uxdroid.system;

import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.content.res.AssetManager;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.os.Looper;
import android.os.UserHandle;
import android.util.DisplayMetrics;
import android.view.LayoutInflater;

/** An app's Context on this host. What it does not override answers with defaults, from DefaultContext. */
public class AppContext extends DefaultContext {
    private final Resources resources;
    private final Resources.Theme theme;
    private final ApplicationInfo info;
    private final ClassLoader classLoader;
    private LayoutInflater inflater;

    public AppContext(Resources resources, ApplicationInfo info, ClassLoader classLoader, int themeResource) {
        this.resources = resources;
        this.info = info;
        this.classLoader = classLoader;
        theme = resources.newTheme();
        theme.applyStyle(themeResource, true);
    }

    /** A context backed only by the framework's resources, for code with no APK of its own. */
    public static AppContext forFramework(String packageName) {
        ApplicationInfo info = new ApplicationInfo();
        info.packageName = packageName;
        info.targetSdkVersion = android.os.Build.VERSION.SDK_INT;
        return new AppContext(systemResources(), info, AppContext.class.getClassLoader(),
                android.R.style.Theme_Material_Light);
    }

    /** The framework's resources for a default 160 dpi, 400 by 700 dp screen. */
    public static Resources systemResources() {
        DisplayMetrics metrics = new DisplayMetrics();
        metrics.setToDefaults();
        Configuration config = new Configuration();
        config.setToDefaults();
        config.densityDpi = metrics.densityDpi;
        config.screenWidthDp = 400;
        config.screenHeightDp = 700;
        return new Resources(AssetManager.getSystem(), metrics, config);
    }

    @Override public Resources getResources() { return resources; }
    @Override public Resources.Theme getTheme() { return theme; }
    @Override public void setTheme(int resid) { theme.applyStyle(resid, true); }
    @Override public ApplicationInfo getApplicationInfo() { return info; }
    @Override public String getPackageName() { return info.packageName; }
    @Override public String getOpPackageName() { return info.packageName; }
    @Override public String getBasePackageName() { return info.packageName; }
    @Override public ClassLoader getClassLoader() { return classLoader; }
    @Override public Context getApplicationContext() { return this; }
    @Override public Looper getMainLooper() { return Looper.getMainLooper(); }
    @Override public int getDisplayId() { return 0; }
    @Override public boolean isUiContext() { return true; }
    @Override public int getUserId() { return 0; }
    @Override public UserHandle getUser() { return android.os.Process.myUserHandle(); }

    // There is one user and no other app to protect from: every permission is granted.
    @Override public int checkPermission(String permission, int pid, int uid) { return PackageManager.PERMISSION_GRANTED; }
    @Override public int checkCallingOrSelfPermission(String permission) { return PackageManager.PERMISSION_GRANTED; }
    @Override public int checkSelfPermission(String permission) { return PackageManager.PERMISSION_GRANTED; }

    @Override
    public Object getSystemService(String name) {
        if (LAYOUT_INFLATER_SERVICE.equals(name)) {
            if (inflater == null) inflater = new com.android.internal.policy.PhoneLayoutInflater(this);
            return inflater;
        }
        return null;
    }

    @Override
    public String getSystemServiceName(Class<?> serviceClass) {
        return serviceClass == LayoutInflater.class ? LAYOUT_INFLATER_SERVICE : null;
    }
}
