package dev.uxdroid.system;

import android.app.Activity;
import android.app.ActivityThread;
import android.app.IActivityClientController;
import android.app.IActivityManager;
import android.app.IActivityTaskManager;
import android.app.IApplicationThread;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.ApplicationInfo;
import android.content.pm.IPackageManager;
import android.content.pm.PackageParser;
import android.content.pm.ProviderInfoList;
import android.content.pm.pkg.FrameworkPackageUserState;
import android.content.res.CompatibilityInfo;
import android.content.res.Configuration;
import android.graphics.Rect;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Looper;
import android.os.RemoteException;
import android.view.View;
import android.view.ViewGroup;
import java.io.File;

/**
 * Starts an APK the way Android does: AOSP's ActivityThread.main attaches to the activity manager, which
 * here is a handler in this process. It answers as Android's would, binding the application and then
 * scheduling a transaction that launches the Activity, so the framework itself makes the LoadedApk, the
 * Application, the Activity, its Context and its PhoneWindow. Without --once the Activity is then
 * resumed, and its window drawn by the framework's ViewRootImpl into a Wayland window.
 *
 * <p>One process runs an app over one data directory, as Android keeps one process per app: the first
 * launch holds a lock and serves a control socket, and a later launch hands its intent to that process
 * through the socket and exits. See {@link Control}.
 */
public final class Launcher {
    private Launcher() {}

    public static void main(String[] args) throws Exception {
        boolean once = args.length > 0 && args[0].equals("--once");
        int first = once ? 1 : 0;
        String givenIntent = null;
        if (args.length > first + 1 && args[first].equals("--intent")) {
            givenIntent = args[first + 1];
            first += 2;
        }
        if (args.length - first < 3 || args.length - first > 4) {
            System.err.println("usage: Launcher [--once] [--intent <json>] <runtime root> <libuxdroid_system.so> <apk>"
                    + " [activity class]");
            System.exit(2);
        }
        String root = args[first];
        File apk = new File(args[first + 2]).getAbsoluteFile();
        String activityName = args.length - first == 4 ? args[first + 3] : null;
        // What uxdroid install prepared beside an installed APK, if it was made for this runtime and file.
        File record = new File(apk.getParentFile(), Record.FILE);
        String identity = Record.identity(root, apk);
        boolean prepared = record.isFile() && Record.matches(record, identity);
        File defaults = new File(apk.getParentFile(), Installer.DEFAULTS);
        Boot.start(root, args[first + 1], prepared && defaults.isFile() ? defaults.getPath() : null);
        org.json.JSONObject intent = new org.json.JSONObject(Control.intent(givenIntent != null ? givenIntent : "{}"));
        if (activityName == null) activityName = intent.optString("activity", null);
        else if (!intent.has("activity")) intent.put("activity", activityName);
        Services.register("permissionmgr", android.permission.IPermissionManager.class, new PermissionManager());
        App app = null;
        if (record.isFile()) {
            try {
                app = App.recorded(apk, Record.read(record, identity), activityName);
            } catch (java.io.IOException | ReflectiveOperationException | RuntimeException e) {
                System.err.println("uxdroid: the install record " + record + " is not used, the APK is parsed instead"
                        + " until uxdroid install runs again: " + e.getMessage());
            }
        }
        if (app == null) app = App.parse(apk, activityName);
        String packageName = app.application.packageName;
        if (!claimInstance(packageName)) {
            if (Control.activated()) refuseActivation(packageName);
            forward(packageName, intent);
        }
        App started = app;
        Permissions.declare(() -> {
            String[] names = started.packageInfo(android.content.pm.PackageManager.GET_PERMISSIONS, 0)
                    .requestedPermissions;
            return names == null ? java.util.List.of() : java.util.Arrays.asList(names);
        });
        Services.register("activity", IActivityManager.class, new ActivityManager(app, once, intent, givenIntent == null));
        Services.register("activity_task", IActivityTaskManager.class, new ActivityTaskManager());
        Services.register("package", IPackageManager.class, new PackageManager(app));
        Services.register("display", android.hardware.display.IDisplayManager.class, new DisplayService());
        Services.register("user", android.os.IUserManager.class, new UserManager());
        Services.register("window", android.view.IWindowManager.class, new WindowService());
        // A window looks the input method service up; with no soft keyboard, every call is counted.
        Services.register("input_method", com.android.internal.view.IInputMethodManager.class, null);
        Services.register("input", android.hardware.input.IInputManager.class, new InputService());
        // Content observers, such as Settings watchers, register here; with no other process to
        // notify them, every call is counted.
        Services.register("content", android.content.IContentService.class, null);
        // Flutter's platform plugin holds the ClipboardManager, which needs the service to exist.
        Services.register("clipboard", android.content.IClipboard.class, null);
        Services.register("audio", android.media.IAudioService.class, new AudioService());
        Services.register("mount", android.os.storage.IStorageManager.class, new StorageService(dataRoot()));
        Services.register("device_identifiers", android.os.IDeviceIdentifiersPolicyService.class,
                new DeviceIdentifiers());
        // The framework's own switch for devices without GPU rendering: ViewRootImpl then draws in
        // software, through Surface.lockCanvas, which libuxdroid_system.so shows on Wayland.
        android.view.ThreadedRenderer.sRendererEnabled = false;
        // Prepares the main Looper, attaches through the "activity" service above, then loops forever.
        ActivityThread.main(new String[0]);
    }

    /**
     * Takes this app's instance lock over the data directory. A lock that cannot be taken at all, such
     * as on a read-only directory, is reported and the app runs anyway.
     */
    private static boolean claimInstance(String packageName) {
        File lock = new File(dataRoot(), "app/." + packageName + ".running");
        try {
            return Control.claim(lock.getPath());
        } catch (IllegalStateException e) {
            System.err.println("uxdroid: " + packageName + " runs without its instance lock: " + e.getMessage());
            return true;
        }
    }

    /**
     * Exit status of a service start that finds the app already running outside its service; the units
     * uxdroid service writes do not restart on it. The socket systemd passed is this process's own, so
     * the launch cannot be handed over through it.
     */
    static final int EXIT_RUNNING_OUTSIDE_SERVICE = 75;

    private static void refuseActivation(String packageName) {
        System.err.println("uxdroid: " + packageName + " is already running over " + dataRoot()
                + " outside its service; stop that process, then start the service");
        System.exit(EXIT_RUNNING_OUTSIDE_SERVICE);
    }

    /** Another process runs the app over this data directory: it takes this launch, and this one exits. */
    private static void forward(String packageName, org.json.JSONObject intent) {
        try {
            org.json.JSONObject result = new org.json.JSONObject(Control.forward(packageName, intent.toString(), 30_000));
            System.err.println("uxdroid: " + packageName + " is already running as process " + result.optInt("pid")
                    + " over " + dataRoot() + ", so this launch went to it: " + result.optString("outcome") + " "
                    + result.optString("activity"));
            System.exit(0);
        } catch (IllegalStateException | org.json.JSONException e) {
            System.err.println("uxdroid: " + packageName + " is already running over " + dataRoot()
                    + ", and it did not take this launch: " + e.getMessage());
            System.exit(1);
        }
    }

    /** The installed APK: its application and the Activity to launch, from its record or parsed now. */
    static final class App {
        /** PackageManager flags whose answer holds more than the record: components, permissions and such. */
        private static final long BEYOND_RECORD = android.content.pm.PackageManager.GET_ACTIVITIES
                | android.content.pm.PackageManager.GET_RECEIVERS | android.content.pm.PackageManager.GET_SERVICES
                | android.content.pm.PackageManager.GET_PROVIDERS | android.content.pm.PackageManager.GET_INSTRUMENTATION
                | android.content.pm.PackageManager.GET_INTENT_FILTERS | android.content.pm.PackageManager.GET_GIDS
                | android.content.pm.PackageManager.GET_URI_PERMISSION_PATTERNS
                | android.content.pm.PackageManager.GET_PERMISSIONS
                | android.content.pm.PackageManager.GET_CONFIGURATIONS;

        private final File apk;
        private PackageParser.Package parsed;
        private final Record.Contents record;
        final ApplicationInfo application;
        final ActivityInfo activity;

        private App(File apk, PackageParser.Package parsed, Record.Contents record, ApplicationInfo application,
                ActivityInfo activity) {
            this.apk = apk;
            this.parsed = parsed;
            this.record = record;
            this.application = application;
            this.activity = activity;
        }

        /** From the install record, or null if the record holds another Activity than the one asked for. */
        static App recorded(File apk, Record.Contents record, String activityName) {
            if (record.activity == null || (activityName != null
                    && !className(record.application.packageName, activityName).equals(record.activity.name))) {
                return null;
            }
            place(record.application, apk, true);
            return new App(apk, null, record, record.application, record.activity);
        }

        static App parse(File apk, String activityName) throws PackageParser.PackageParserException {
            PackageParser.Package parsed = parsePackage(apk);
            ApplicationInfo application = applicationInfo(parsed, apk, true);
            ActivityInfo activity = activity(parsed, application, activityName);
            if (activity == null) {
                throw new IllegalArgumentException(activityName != null
                        ? apk + " declares no activity " + activityName
                        : apk + " declares no launcher activity");
            }
            return new App(apk, parsed, null, application, activity);
        }

        /** The framework's own manifest parser, so the app sees the values Android would give it. */
        static PackageParser.Package parsePackage(File apk) throws PackageParser.PackageParserException {
            return new PackageParser().parsePackage(apk, 0);
        }

        static ApplicationInfo applicationInfo(PackageParser.Package parsed, File apk, boolean create) {
            ApplicationInfo application = PackageParser.generateApplicationInfo(parsed, 0,
                    FrameworkPackageUserState.DEFAULT, 0);
            place(application, apk, create);
            return application;
        }

        /** The named Activity, or the launcher Activity; null if there is none. */
        static ActivityInfo activity(PackageParser.Package parsed, ApplicationInfo application, String activityName) {
            for (PackageParser.Activity candidate : parsed.activities) {
                boolean chosen = activityName != null
                        ? candidate.className.equals(className(parsed.packageName, activityName))
                        : candidate.intents.stream().anyMatch(filter -> filter.hasAction(Intent.ACTION_MAIN)
                                && filter.hasCategory(Intent.CATEGORY_LAUNCHER));
                if (chosen) {
                    ActivityInfo activity = PackageParser.generateActivityInfo(candidate, 0,
                            FrameworkPackageUserState.DEFAULT, 0);
                    activity.applicationInfo = application;
                    return activity;
                }
            }
            return null;
        }

        /** Where the installer put the app: its code is the APK, its data under ANDROID_DATA. */
        private static void place(ApplicationInfo application, File apk, boolean create) {
            String path = apk.getAbsolutePath();
            application.sourceDir = path;
            application.publicSourceDir = path;
            File data = new File(dataRoot(), "data/" + application.packageName);
            File libraries = new File(data, "lib");
            if (create && !libraries.isDirectory() && !libraries.mkdirs()) {
                throw new IllegalStateException("cannot create " + libraries);
            }
            application.dataDir = data.getPath();
            application.credentialProtectedDataDir = data.getPath();
            application.deviceProtectedDataDir = data.getPath();
            application.nativeLibraryDir = libraries.getPath();
            application.uid = android.os.Process.myUid();
            if (application.processName == null) application.processName = application.packageName;
        }

        /** What PackageManager.getPackageInfo answers; parses the APK only when the record holds too little. */
        android.content.pm.PackageInfo packageInfo(long flags, int userId) {
            if (record != null && (flags & BEYOND_RECORD) == 0) return record.packageInfo();
            return PackageParser.generatePackageInfo(parsed(), new int[0], (int) flags, 0, 0,
                    java.util.Collections.emptySet(), FrameworkPackageUserState.DEFAULT, userId);
        }

        /** The framework's parse of the APK, made the first time something needs more than the record. */
        PackageParser.Package parsed() {
            if (parsed == null) {
                try {
                    parsed = parsePackage(apk);
                } catch (PackageParser.PackageParserException e) {
                    throw new IllegalStateException("cannot parse " + apk, e);
                }
            }
            return parsed;
        }

        ActivityInfo activityInfo(PackageParser.Activity declared) {
            ActivityInfo info = PackageParser.generateActivityInfo(declared, 0, FrameworkPackageUserState.DEFAULT, 0);
            info.applicationInfo = application;
            return info;
        }

        ComponentName component() {
            return new ComponentName(activity.packageName, activity.name);
        }
    }

    /** A class name as a manifest gives it: {@code .Name} and {@code Name} are in the package. */
    static String className(String packageName, String name) {
        return name.startsWith(".") ? packageName + name : name.contains(".") ? name : packageName + "." + name;
    }

    /** Android's /data: where the installer put the app, and where its data goes. */
    static String dataRoot() {
        String root = System.getenv("ANDROID_DATA");
        if (root == null || !new File(root).isAbsolute()) {
            throw new IllegalStateException("ANDROID_DATA must be an absolute directory, not " + root);
        }
        return root;
    }

    /** The handler of IActivityManager: attaching the process is what starts the app. */
    public static final class ActivityManager {
        private final App app;
        private final boolean once;
        private final org.json.JSONObject intent;
        private final boolean launcherIntent;
        private final java.util.Map<String, android.content.IContentProvider> providers = new java.util.HashMap<>();

        /**
         * The display's toplevel was closed, as a desktop closes a task: every activity is destroyed as
         * finishing, then the process exits, the same path as uxdroid app stop.
         */
        static void closeTask() {
            ActivityStack stack = ActivityStack.current;
            if (stack == null) System.exit(0);
            try {
                stack.stop();
            } catch (RemoteException | org.json.JSONException e) {
                throw new IllegalStateException("cannot close the task", e);
            }
        }

        /** @param launcherIntent the launch names at most an activity, so it gets the launcher's intent */
        ActivityManager(App app, boolean once, org.json.JSONObject intent, boolean launcherIntent) {
            this.app = app;
            this.once = once;
            this.intent = intent;
            this.launcherIntent = launcherIntent;
        }

        public void attachApplication(IApplicationThread thread, long startSeq) throws RemoteException {
            Configuration config = configuration();
            ApplicationInfo info = app.application;
            useFrameworkAsParent(info);
            thread.bindApplication(info.processName, info,
                    null, null, false, // not an SDK sandbox
                    ProviderInfoList.fromList(java.util.Collections.emptyList()),
                    null, null, null, null, null, // no instrumentation, profiler or UI automation
                    0, false, false, false, false, // debug mode, binder tracking, allocation tracking, backup, persistent
                    config, CompatibilityInfo.DEFAULT_COMPATIBILITY_INFO,
                    null, new Bundle(), // services come from the service manager; no core settings
                    android.os.Build.UNKNOWN, null, null,
                    new long[0], new long[0], // no compatibility changes disabled or logged
                    null, sharedMemory(), // fonts are already loaded by Boot
                    android.os.SystemClock.elapsedRealtime(), android.os.SystemClock.uptimeMillis());

            // Created only with --once: a report needs no window, so it runs with no compositor.
            ActivityStack stack = new ActivityStack(app, thread, !once);
            ActivityStack.current = stack;
            Control.start(info.packageName);
            try {
                Intent launch = Control.intentOf(intent);
                if (launcherIntent) launch.addCategory(Intent.CATEGORY_LAUNCHER);
                stack.start(launch, stack.resolve(launch, intent.optString("activity", null)));
            } catch (ActivityStack.Refusal | org.json.JSONException e) {
                throw new IllegalArgumentException("cannot launch " + info.packageName + ": " + e.getMessage(), e);
            }
            if (once) exitWhenLaunched(stack.top());
        }

        /**
         * Providers of the system, such as Settings, live in this process as proxies of IContentProvider.
         * A call nobody handles returns null, which Settings reads as "use the default".
         */
        public android.app.ContentProviderHolder getContentProvider(IApplicationThread caller, String callingPackage,
                String authority, int userId, boolean stable) {
            android.content.pm.ProviderInfo info = new android.content.pm.ProviderInfo();
            info.authority = authority;
            info.name = authority;
            info.packageName = "android";
            info.applicationInfo = new ApplicationInfo();
            info.applicationInfo.packageName = "android";
            info.applicationInfo.uid = android.os.Process.SYSTEM_UID;
            android.app.ContentProviderHolder holder = new android.app.ContentProviderHolder(info);
            holder.provider = providers.computeIfAbsent(authority,
                    name -> Services.local(android.content.IContentProvider.class, null));
            holder.noReleaseNeeded = true;
            return holder;
        }
    }

    // ActivityThread caches the LoadedApk by weak reference only; this keeps it until the bind holds it.
    private static Object loadedApk;

    /**
     * Android has the framework on the boot class path, which every app's class loader reaches. Here it
     * is on the system class path, so the app's LoadedApk must have that loader as its parent. Caching
     * the LoadedApk made that way, before the bind runs, makes the framework's own bind find and use it.
     */
    private static void useFrameworkAsParent(ApplicationInfo info) {
        try {
            java.lang.reflect.Method packageInfo = ActivityThread.class.getDeclaredMethod("getPackageInfo",
                    ApplicationInfo.class, CompatibilityInfo.class, ClassLoader.class,
                    boolean.class, boolean.class, boolean.class, boolean.class);
            packageInfo.setAccessible(true);
            // Base loader, then as the bind asks: no security violation, include code, do not register, no sandbox.
            loadedApk = packageInfo.invoke(ActivityThread.currentActivityThread(), info, CompatibilityInfo.DEFAULT_COMPATIBILITY_INFO,
                    Launcher.class.getClassLoader(), false, true, false, false);
        } catch (ReflectiveOperationException e) {
            throw new IllegalStateException("cannot give the app the framework's class loader", e);
        }
    }

    /** The handler of IUserManager: one user, always running and unlocked. */
    public static final class UserManager {
        public boolean isUserUnlocked(int userId) { return userId == 0; }
        public boolean isUserRunning(int userId) { return userId == 0; }
        // Credential-protected storage, such as SharedPreferences, opens only once this is true.
        public boolean isUserUnlockingOrUnlocked(int userId) { return userId == 0; }
    }

    static IActivityClientController clientController() {
        return CLIENT_CONTROLLER;
    }

    /** What an Activity asks about its task and window. Other calls are counted. */
    private static final IActivityClientController CLIENT_CONTROLLER =
            Services.local(IActivityClientController.class, new ActivityClientController());

    /** The handler of IActivityClientController. */
    public static final class ActivityClientController {
        /** Activity.finish: the task drops it and resumes the one below. */
        public boolean finishActivity(IBinder token, int resultCode, Intent resultData, int finishTask)
                throws RemoteException {
            return ActivityStack.current != null && ActivityStack.current.finish(token);
        }

        /**
         * Activity.reportFullyDrawn: an app's own signal that its first full frame is on screen. Flutter's
         * activity sends it once its first frame is rendered, so tests wait for this line.
         */
        public void reportActivityFullyDrawn(IBinder token, boolean restoredFromBundle) {
            Activity activity = ActivityThread.currentActivityThread().getActivity(token);
            System.err.println("uxdroid: activity fully drawn: "
                    + (activity == null ? "unknown" : activity.getComponentName().flattenToShortString()));
        }
    }

    /** The handler of IActivityTaskManager. */
    public static final class ActivityTaskManager {
        public IActivityClientController getActivityClientController() {
            return CLIENT_CONTROLLER;
        }
    }

    /** The handler of IPermissionManager. */
    public static final class PermissionManager {
        // The manifest parser asks which permissions an old target SDK implies; none are split here.
        public java.util.List<?> getSplitPermissions() {
            return java.util.Collections.emptyList();
        }
    }

    /** The handler of IPackageManager: knows the one installed app. */
    public static final class PackageManager {
        private final App app;

        PackageManager(App app) {
            this.app = app;
        }

        public ApplicationInfo getApplicationInfo(String packageName, long flags, int userId) {
            return packageName.equals(app.application.packageName) ? app.application : null;
        }

        public ActivityInfo getActivityInfo(ComponentName component, long flags, int userId) {
            if (component.equals(app.component())) return app.activity;
            // Activities started since, which the APK was parsed for; others are not looked up.
            return ActivityStack.current != null ? ActivityStack.current.info(component) : null;
        }

        public android.content.pm.PackageInfo getPackageInfo(String packageName, long flags, int userId) {
            if (!packageName.equals(app.application.packageName)) return null;
            android.content.pm.PackageInfo info = app.packageInfo(flags, userId);
            info.applicationInfo = app.application;
            if (info.activities != null) {
                for (ActivityInfo activity : info.activities) activity.applicationInfo = app.application;
            }
            return info;
        }

        /** Installed from a file, as adb would: no installer, initiator or origin. */
        public android.content.pm.InstallSourceInfo getInstallSourceInfo(String packageName, int userId) {
            if (!packageName.equals(app.application.packageName)) return null;
            return new android.content.pm.InstallSourceInfo(null, null, null, null);
        }

        public String[] getPackagesForUid(int uid) {
            return uid == app.application.uid ? new String[] {app.application.packageName} : null;
        }
    }

    /**
     * The handler of IDeviceIdentifiersPolicyService. An app without the privileged phone permission
     * gets {@code Build.UNKNOWN} as the serial number on Android too.
     */
    public static final class DeviceIdentifiers {
        public String getSerial() { return android.os.Build.UNKNOWN; }
        public String getSerialForPackage(String callingPackage, String callingFeatureId) {
            return android.os.Build.UNKNOWN;
        }
    }

    // The system's side of the memory it shares with the app, kept mapped for the process's life.
    private static com.android.internal.os.ApplicationSharedMemory systemShared;

    /** What system_server gives each app: a read-only descriptor of memory the system writes. */
    private static java.io.FileDescriptor sharedMemory() {
        if (systemShared == null) systemShared = com.android.internal.os.ApplicationSharedMemory.create();
        try {
            return systemShared.getReadOnlyFileDescriptor();
        } catch (java.io.IOException e) {
            throw new IllegalStateException("cannot share memory with the app", e);
        }
    }

    static Configuration configuration() {
        Configuration config = new Configuration(AppContext.systemResources().getConfiguration());
        Rect bounds = new Rect(0, 0, DisplayService.widthPixels(), DisplayService.heightPixels());
        config.windowConfiguration.setBounds(bounds);
        config.windowConfiguration.setAppBounds(bounds);
        config.windowConfiguration.setMaxBounds(bounds);
        int density = config.densityDpi > 0 ? config.densityDpi : DisplayService.DENSITY;
        config.screenWidthDp = bounds.width() * android.util.DisplayMetrics.DENSITY_DEFAULT / density;
        config.screenHeightDp = bounds.height() * android.util.DisplayMetrics.DENSITY_DEFAULT / density;
        config.smallestScreenWidthDp = Math.min(config.screenWidthDp, config.screenHeightDp);
        config.orientation = bounds.width() > bounds.height()
                ? Configuration.ORIENTATION_LANDSCAPE : Configuration.ORIENTATION_PORTRAIT;
        return config;
    }

    /** For tests: once the Activity exists and the main thread is idle, report it and exit. */
    private static void exitWhenLaunched(IBinder token) {
        Looper.myQueue().addIdleHandler(() -> {
            Activity activity = ActivityThread.currentActivityThread().getActivity(token);
            if (activity == null) return true;
            report(activity);
            System.exit(0);
            return false;
        });
    }

    private static void report(Activity activity) {
        System.out.println("launched " + activity.getComponentName().flattenToShortString()
                + " in " + activity.getApplication().getClass().getName());
        System.out.println("window " + activity.getWindow().getClass().getName());
        View decor = activity.getWindow().getDecorView();
        Rect bounds = configuration().windowConfiguration.getBounds();
        decor.measure(View.MeasureSpec.makeMeasureSpec(bounds.width(), View.MeasureSpec.EXACTLY),
                View.MeasureSpec.makeMeasureSpec(bounds.height(), View.MeasureSpec.EXACTLY));
        decor.layout(0, 0, decor.getMeasuredWidth(), decor.getMeasuredHeight());
        View content = ((ViewGroup) activity.findViewById(android.R.id.content)).getChildAt(0);
        System.out.println("content " + content.getClass().getName() + " " + content.getWidth() + "x" + content.getHeight());
        System.out.println("unhandled " + Services.unhandled());
    }
}
