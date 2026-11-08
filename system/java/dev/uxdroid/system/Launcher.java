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
        // Only the instance that runs the app takes its identity, before ActivityThread runs: the
        // framework reads the app's uid once and keeps it.
        Natives.becomeApplication(packageName);
        App started = app;
        Permissions.declare(() -> {
            String[] names = started.packageInfo(android.content.pm.PackageManager.GET_PERMISSIONS, 0)
                    .requestedPermissions;
            return names == null ? java.util.List.of() : java.util.Arrays.asList(names);
        });
        Services.register("activity", IActivityManager.class, new ActivityManager(app, once, intent, givenIntent == null));
        Services.register("activity_task", IActivityTaskManager.class, new ActivityTaskManager());
        Packages.start(root, app);
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
        AppServices.register(app);
        StoreServices.register(app);
        // The framework's own switch for devices without GPU rendering: ViewRootImpl then draws in
        // software, through Surface.lockCanvas, which libuxdroid_system.so shows on Wayland.
        android.view.ThreadedRenderer.sRendererEnabled = false;
        Crashes.install();
        // Prepares the main Looper, attaches through the "activity" service above, then loops forever.
        try {
            ActivityThread.main(new String[0]);
        } catch (Throwable e) {
            // dalvikvm would hold the exception until every other thread ends, which an app's threads
            // never do: the handlers run now, as for any other thread, and the process ends even if the
            // app's handler does not pass the exception on, since nothing runs without the main thread.
            Thread main = Thread.currentThread();
            Crashes.log(main, e);
            try {
                main.getUncaughtExceptionHandler().uncaughtException(main, e);
            } finally {
                Crashes.exit();
            }
        }
    }

    /**
     * What Android's RuntimeInit does for an exception nothing catches: before any handler, it is
     * logged as "FATAL EXCEPTION" under AndroidRuntime, then the app's own handlers run, and the last
     * one in their chain, ours, ends the process. Without it a dead main thread leaves the process
     * running, unable to draw or answer, with nothing in the log.
     */
    static final class Crashes {
        private static final String TAG = "AndroidRuntime";
        private static volatile boolean crashing;

        static void install() {
            Thread.UncaughtExceptionHandler log = (thread, error) -> log(thread, error);
            // libcore's hook that runs before the thread's handler, which apps cannot replace.
            try {
                Thread.class.getMethod("setUncaughtExceptionPreHandler", Thread.UncaughtExceptionHandler.class)
                        .invoke(null, log);
            } catch (ReflectiveOperationException e) {
                throw new IllegalStateException("libcore has no Thread.setUncaughtExceptionPreHandler", e);
            }
            Thread.setDefaultUncaughtExceptionHandler((thread, error) -> {
                try {
                    log(thread, error); // logs once: here only when the pre-handler did not run
                } finally {
                    exit();
                }
            });
        }

        static void exit() {
            android.os.Process.killProcess(android.os.Process.myPid());
            System.exit(10);
        }

        static void log(Thread thread, Throwable error) {
            if (crashing) return; // a second exception while the first ends the process
            crashing = true;
            java.io.StringWriter trace = new java.io.StringWriter();
            error.printStackTrace(new java.io.PrintWriter(trace));
            StringBuilder text = new StringBuilder("FATAL EXCEPTION: ").append(thread.getName())
                    .append("\nProcess: ").append(currentPackage()).append(", PID: ")
                    .append(android.os.Process.myPid()).append('\n').append(trace);
            // Line by line, as logcat shows a multi-line message.
            for (String line : text.toString().split("\n")) android.util.Log.e(TAG, line);
        }

        private static String currentPackage() {
            String name = ActivityThread.currentPackageName();
            return name != null ? name : "?";
        }
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

        /** Flags whose answer the record holds: the components, each with its meta-data. */
        private static final long COMPONENTS = android.content.pm.PackageManager.GET_ACTIVITIES
                | android.content.pm.PackageManager.GET_RECEIVERS | android.content.pm.PackageManager.GET_SERVICES
                | android.content.pm.PackageManager.GET_PROVIDERS;

        private final File apk;
        private PackageParser.Package parsed;
        private final Record.Contents record;
        private Components components;
        final ApplicationInfo application;
        /** The Activity to launch, or null for an app known only to the package manager, not started. */
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
            return new App(apk, null, record, record.application,
                    Components.withFlags(record.activity, 0, record.application));
        }

        /**
         * Another app installed over the same data directory, known to the package manager but not
         * started: from its record when it is valid for {@code root}, else from the framework's parse.
         */
        static App installed(String root, File apk) throws PackageParser.PackageParserException {
            File file = new File(apk.getParentFile(), Record.FILE);
            String identity = Record.identity(root, apk);
            if (file.isFile() && Record.matches(file, identity)) {
                try {
                    Record.Contents record = Record.read(file, identity);
                    place(record.application, apk, false);
                    return new App(apk, null, record, record.application, null);
                } catch (java.io.IOException | ReflectiveOperationException | RuntimeException e) {
                    System.err.println("uxdroid: the install record " + file + " is not used, the APK is parsed instead: "
                            + e.getMessage());
                }
            }
            PackageParser.Package parsed = parsePackage(apk);
            return new App(apk, parsed, null, applicationInfo(parsed, apk, false), null);
        }

        String packageName() {
            return application.packageName;
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

        /** What a recorded provider holds: all a bind or a provider lookup can ask for. */
        static final int PROVIDER_FLAGS = android.content.pm.PackageManager.GET_META_DATA
                | android.content.pm.PackageManager.GET_URI_PERMISSION_PATTERNS;

        /** Every provider the manifest declares, as the package manager describes it, of this application. */
        static android.content.pm.ProviderInfo[] providers(PackageParser.Package parsed, ApplicationInfo application) {
            android.content.pm.ProviderInfo[] providers = new android.content.pm.ProviderInfo[parsed.providers.size()];
            for (int i = 0; i < providers.length; i++) {
                providers[i] = PackageParser.generateProviderInfo(parsed.providers.get(i), PROVIDER_FLAGS,
                        FrameworkPackageUserState.DEFAULT, 0);
                providers[i].applicationInfo = application;
            }
            return providers;
        }

        /** The app's declared providers, from its record or its parse; see {@link Providers}. */
        android.content.pm.ProviderInfo[] providers() {
            return components().providers;
        }

        /** Every declared component with its filters, from the record or the framework's parse. */
        synchronized Components components() {
            if (components == null) components = record != null ? record.components : Components.of(parsed(), application);
            return components;
        }

        /** The application's meta-data from the manifest, or null. */
        android.os.Bundle metaData() {
            return record != null ? record.metaData : parsed().mAppMetaData;
        }

        /** What PackageManager.getApplicationInfo answers: a copy, with meta-data only for GET_META_DATA. */
        ApplicationInfo applicationInfo(long flags) {
            ApplicationInfo info = new ApplicationInfo(application);
            android.os.Bundle metaData = (flags & android.content.pm.PackageManager.GET_META_DATA) != 0 ? metaData() : null;
            info.metaData = metaData == null ? null : new android.os.Bundle(metaData);
            return info;
        }

        /**
         * The framework's own manifest parser, so the app sees the values Android would give it. An app
         * installed with splits is parsed as Android parses its code directory: base.apk and every
         * split_*.apk beside it as one package.
         */
        static PackageParser.Package parsePackage(File apk) throws PackageParser.PackageParserException {
            return new PackageParser().parsePackage(splits(apk).length > 0 ? apk.getParentFile() : apk, 0);
        }

        /** The splits uxdroid install put beside an installed base.apk, by name, as its splits file lists them. */
        static String[] splits(File apk) {
            File list = new File(apk.getParentFile(), SPLITS);
            if (!apk.getName().equals("base.apk") || !list.isFile()) return new String[0];
            try {
                return java.nio.file.Files.readAllLines(list.toPath()).stream()
                        .filter(name -> !name.isEmpty()).toArray(String[]::new);
            } catch (java.io.IOException e) {
                throw new IllegalStateException("cannot read " + list, e);
            }
        }

        /** The file uxdroid install lists an app's splits in, beside base.apk. */
        static final String SPLITS = "splits";

        /** Where a split of an installed base.apk is. */
        static File split(File apk, String name) {
            return new File(apk.getParentFile(), "split_" + name + ".apk");
        }

        static ApplicationInfo applicationInfo(PackageParser.Package parsed, File apk, boolean create) {
            ApplicationInfo application = PackageParser.generateApplicationInfo(parsed, 0,
                    FrameworkPackageUserState.DEFAULT, 0);
            place(application, apk, create);
            return application;
        }

        /**
         * The named Activity, or the launcher Activity: the first one the manifest leaves enabled, as a
         * home screen shows only those, such as F-Droid's real screen rather than its disabled disguise.
         * Null if there is none.
         */
        static ActivityInfo activity(PackageParser.Package parsed, ApplicationInfo application, String activityName) {
            for (PackageParser.Activity candidate : parsed.activities) {
                boolean chosen = activityName != null
                        ? candidate.className.equals(className(parsed.packageName, activityName))
                        : candidate.info.enabled && candidate.intents.stream().anyMatch(filter -> filter.hasAction(
                                Intent.ACTION_MAIN) && filter.hasCategory(Intent.CATEGORY_LAUNCHER));
                if (chosen) {
                    ActivityInfo activity = PackageParser.generateActivityInfo(candidate, 0,
                            FrameworkPackageUserState.DEFAULT, 0);
                    activity.applicationInfo = application;
                    return activity;
                }
            }
            return null;
        }

        /**
         * Where the installer put the app: its code is the APK and its splits, its data under ANDROID_DATA.
         * The framework's LoadedApk adds the splits, which are not isolated, to the app's class loader
         * after the base, in this order, which is the one install compiled them in, and their resources
         * to the app's.
         */
        private static void place(ApplicationInfo application, File apk, boolean create) {
            String path = apk.getAbsolutePath();
            application.sourceDir = path;
            application.publicSourceDir = path;
            String[] splits = splits(apk);
            String[] paths = new String[splits.length];
            for (int i = 0; i < splits.length; i++) paths[i] = split(apk, splits[i]).getAbsolutePath();
            application.splitNames = splits.length == 0 ? null : splits;
            application.splitSourceDirs = splits.length == 0 ? null : paths;
            application.splitPublicSourceDirs = splits.length == 0 ? null : paths.clone();
            File data = new File(dataRoot(), "data/" + application.packageName);
            File libraries = new File(data, "lib");
            if (create && !libraries.isDirectory() && !libraries.mkdirs()) {
                throw new IllegalStateException("cannot create " + libraries);
            }
            application.dataDir = data.getPath();
            application.credentialProtectedDataDir = data.getPath();
            application.deviceProtectedDataDir = data.getPath();
            application.nativeLibraryDir = libraries.getPath();
            // An application uid of its own, as Android's installer gives, not the Linux user's.
            application.uid = Natives.applicationUid(application.packageName);
            if (application.processName == null) application.processName = application.packageName;
        }

        /**
         * What PackageManager.getPackageInfo answers, with this app's ApplicationInfo; parses the APK only
         * when the record holds too little. Components come from the record, enabled ones unless
         * MATCH_DISABLED_COMPONENTS, as the framework's own generatePackageInfo lists them.
         */
        android.content.pm.PackageInfo packageInfo(long flags, int userId) {
            ApplicationInfo placed = applicationInfo(flags);
            android.content.pm.PackageInfo info;
            if (record != null && (flags & BEYOND_RECORD & ~COMPONENTS) == 0) {
                info = record.packageInfo();
                Components declared = components();
                if ((flags & android.content.pm.PackageManager.GET_ACTIVITIES) != 0) {
                    info.activities = array(Components.listed(declared.activities, this, flags, placed), ActivityInfo.class);
                }
                if ((flags & android.content.pm.PackageManager.GET_RECEIVERS) != 0) {
                    info.receivers = array(Components.listed(declared.receivers, this, flags, placed), ActivityInfo.class);
                }
                if ((flags & android.content.pm.PackageManager.GET_SERVICES) != 0) {
                    info.services = array(Components.listed(declared.services, this, flags, placed),
                            android.content.pm.ServiceInfo.class);
                }
                if ((flags & android.content.pm.PackageManager.GET_PROVIDERS) != 0) {
                    info.providers = array(Components.listed(declared.providers, this, flags, placed),
                            android.content.pm.ProviderInfo.class);
                }
            } else {
                info = PackageParser.generatePackageInfo(parsed(), new int[0], (int) flags, 0, 0,
                        java.util.Collections.emptySet(), FrameworkPackageUserState.DEFAULT, userId);
                for (android.content.pm.ComponentInfo[] kind : new android.content.pm.ComponentInfo[][] {
                        info.activities, info.receivers, info.services, info.providers}) {
                    if (kind != null) for (android.content.pm.ComponentInfo component : kind) component.applicationInfo = placed;
                }
            }
            info.applicationInfo = placed;
            return info;
        }

        private static <T> T[] array(java.util.List<T> list, Class<T> type) {
            return list == null ? null : list.toArray((T[]) java.lang.reflect.Array.newInstance(type, list.size()));
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
                    ProviderInfoList.fromList(Providers.atBind(app)), // installed before Application.onCreate
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
            Broadcasts.attach(app, thread);
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

        /** Context.registerReceiver: see {@link Broadcasts}. */
        public Intent registerReceiverWithFeature(IApplicationThread caller, String callerPackage, String featureId,
                String receiverId, android.content.IIntentReceiver receiver, android.content.IntentFilter filter,
                String permission, int userId, int flags) throws RemoteException {
            return Broadcasts.register(receiver, filter);
        }

        /**
         * The app's one process, in the foreground: apps such as Instagram find their main process here
         * by name before they initialize.
         */
        public java.util.List<android.app.ActivityManager.RunningAppProcessInfo> getRunningAppProcesses() {
            android.app.ActivityManager.RunningAppProcessInfo info = new android.app.ActivityManager.RunningAppProcessInfo(
                    app.application.processName, android.os.Process.myPid(), new String[] {app.application.packageName});
            info.uid = app.application.uid;
            info.importance = android.app.ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND;
            return java.util.List.of(info);
        }

        /** The framework publishes the providers it installed; they are already in the only process. */
        public void publishContentProviders(IApplicationThread caller,
                java.util.List<android.app.ContentProviderHolder> providers) {}

        public void unregisterReceiver(android.content.IIntentReceiver receiver) {
            Broadcasts.unregister(receiver);
        }

        /** Context.sendBroadcast and its ordered and sticky kinds, to the app's own receivers. */
        public int broadcastIntentWithFeature(IApplicationThread caller, String featureId, Intent intent,
                String resolvedType, android.content.IIntentReceiver resultTo, int resultCode, String resultData,
                Bundle resultExtras, String[] permissions, String[] excludedPermissions, String[] excludedPackages,
                int appOp, Bundle options, boolean serialized, boolean sticky, int userId) throws RemoteException {
            return Broadcasts.send(new Intent(intent), resultTo, resultCode, resultData, resultExtras, serialized, sticky);
        }

        public void finishReceiver(IBinder who, int resultCode, String resultData, Bundle resultExtras,
                boolean abort, int flags) throws RemoteException {
            Broadcasts.finished(who, resultCode, resultData, resultExtras, abort);
        }

        /** PendingIntent.getActivity, getBroadcast and their kind: see {@link IntentSenders}. */
        public android.content.IIntentSender getIntentSenderWithFeature(int type, String packageName,
                String featureId, IBinder token, String resultWho, int requestCode, Intent[] intents,
                String[] resolvedTypes, int flags, Bundle options, int userId) {
            return IntentSenders.get(type, packageName, requestCode, intents, flags);
        }

        public void cancelIntentSender(android.content.IIntentSender sender) {
            IntentSenders.cancel(sender);
        }

        public int sendIntentSender(IApplicationThread caller, android.content.IIntentSender target,
                IBinder allowlistToken, int code, Intent intent, String resolvedType,
                android.content.IIntentReceiver finishedReceiver, String requiredPermission, Bundle options) {
            return IntentSenders.send(target, intent);
        }

        public android.app.ActivityManager.PendingIntentInfo getInfoForIntentSender(
                android.content.IIntentSender sender) {
            return IntentSenders.info(sender, app.application.uid);
        }

        public Intent getIntentForIntentSender(android.content.IIntentSender sender) {
            return IntentSenders.intent(sender);
        }

        /**
         * The app's own providers are made in this process, by the framework, from the info this gives
         * it; see {@link Providers}. Providers of the system, such as Settings, live in this process as
         * proxies of IContentProvider. A call nobody handles returns null, which Settings reads as "use
         * the default".
         */
        public android.app.ContentProviderHolder getContentProvider(IApplicationThread caller, String callingPackage,
                String authority, int userId, boolean stable) {
            android.app.ContentProviderHolder own = Providers.holder(app, authority);
            if (own != null) return own;
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
            // A LoadedApk looks the manifest's appComponentFactory up on its base loader as it is made,
            // which on Android is null and skips it, and again on the app's loader once that exists.
            // Here the base is the framework's, which cannot hold the app's class, so the factory is
            // hidden until the app's loader looks it up.
            String factory = info.appComponentFactory;
            info.appComponentFactory = null;
            try {
                // Base loader, then as the bind asks: no security violation, include code, do not register, no sandbox.
                loadedApk = packageInfo.invoke(ActivityThread.currentActivityThread(), info,
                        CompatibilityInfo.DEFAULT_COMPATIBILITY_INFO, Launcher.class.getClassLoader(),
                        false, true, false, false);
            } finally {
                info.appComponentFactory = factory;
            }
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
        /** Activity.finish: the task drops it, hands its result to who asked, and resumes the one below. */
        public boolean finishActivity(IBinder token, int resultCode, Intent resultData, int finishTask)
                throws RemoteException {
            return ActivityStack.current != null && ActivityStack.current.finish(token, resultCode, resultData);
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

        /** The app's own Context.startActivity and Activity.startActivity: its task decides. */
        public int startActivity(IApplicationThread caller, String callingPackage, String callingFeatureId,
                Intent intent, String resolvedType, IBinder resultTo, String resultWho, int requestCode, int flags,
                android.app.ProfilerInfo profilerInfo, Bundle options) throws RemoteException {
            ActivityStack stack = ActivityStack.current;
            if (stack == null) return android.app.ActivityManager.START_INTENT_NOT_RESOLVED;
            int started = stack.startFromAnyThread(intent, ActivityStack.Result.of(resultTo, resultWho, requestCode));
            if (started == android.app.ActivityManager.START_INTENT_NOT_RESOLVED
                    || started == android.app.ActivityManager.START_CLASS_NOT_FOUND) {
                Packages.refuseOtherApp(intent, resolvedType);
            }
            return started;
        }

        /**
         * Context.startActivities, as TaskStackBuilder and Navigation's deep links start a back stack:
         * each intent is started in order through the task, so the last is on top. Android makes only
         * the top one at once and the others when the user goes back; here each is made as it starts.
         * The first that fails stops the rest, with its code.
         */
        public int startActivities(IApplicationThread caller, String callingPackage, String callingFeatureId,
                Intent[] intents, String[] resolvedTypes, IBinder resultTo, Bundle options, int userId)
                throws RemoteException {
            int started = android.app.ActivityManager.START_SUCCESS;
            for (int i = 0; i < intents.length; i++) {
                started = startActivity(caller, callingPackage, callingFeatureId, intents[i],
                        resolvedTypes == null ? null : resolvedTypes[i], i == 0 ? resultTo : null, null, -1, 0, null,
                        options);
                if (started < 0) return started;
            }
            return started;
        }
    }

    /** The handler of IPermissionManager. */
    public static final class PermissionManager {
        // The manifest parser asks which permissions an old target SDK implies; none are split here.
        public java.util.List<?> getSplitPermissions() {
            return java.util.Collections.emptyList();
        }
    }

    /**
     * The handler of IPackageManager: knows every app installed over the data directory, through
     * {@link Packages}, so one app can look another up, resolve intents across apps and list them.
     */
    public static final class PackageManager {
        private final App app;
        private final android.content.pm.IPackageInstaller installer = StoreServices.packageInstaller();

        PackageManager(App app) {
            this.app = app;
        }

        /** Sessions are not served yet: the installer lists none and refuses to create one. */
        public android.content.pm.IPackageInstaller getPackageInstaller() {
            return installer;
        }

        /** As androidx-startup reads its initializers from its provider's meta-data. */
        public android.content.pm.ProviderInfo getProviderInfo(ComponentName component, long flags, int userId) {
            if (component.getPackageName().equals(app.packageName())) return Providers.info(app, component, flags);
            return (android.content.pm.ProviderInfo) Packages.info(Packages.Kind.PROVIDER, component, flags);
        }

        public android.content.pm.ProviderInfo resolveContentProvider(String authority, long flags, int userId) {
            return Providers.resolve(app, authority, flags);
        }

        public ApplicationInfo getApplicationInfo(String packageName, long flags, int userId) {
            return Packages.application(packageName, flags);
        }

        public ActivityInfo getActivityInfo(ComponentName component, long flags, int userId) {
            return (ActivityInfo) Packages.info(Packages.Kind.ACTIVITY, component, flags);
        }

        public ActivityInfo getReceiverInfo(ComponentName component, long flags, int userId) {
            return (ActivityInfo) Packages.info(Packages.Kind.RECEIVER, component, flags);
        }

        public android.content.pm.ServiceInfo getServiceInfo(ComponentName component, long flags, int userId) {
            return (android.content.pm.ServiceInfo) Packages.info(Packages.Kind.SERVICE, component, flags);
        }

        public android.content.pm.PackageInfo getPackageInfo(String packageName, long flags, int userId) {
            App installed = Packages.get(packageName);
            return installed == null ? null : installed.packageInfo(flags, userId);
        }

        public android.content.pm.PackageInfo getPackageInfoVersioned(android.content.pm.VersionedPackage versioned,
                long flags, int userId) {
            return getPackageInfo(versioned.getPackageName(), flags, userId);
        }

        public android.content.pm.ParceledListSlice<android.content.pm.PackageInfo> getInstalledPackages(long flags,
                int userId) {
            java.util.List<android.content.pm.PackageInfo> list = new java.util.ArrayList<>();
            for (App installed : Packages.all()) list.add(installed.packageInfo(flags, userId));
            return new android.content.pm.ParceledListSlice<>(list);
        }

        public android.content.pm.ParceledListSlice<ApplicationInfo> getInstalledApplications(long flags, int userId) {
            java.util.List<ApplicationInfo> list = new java.util.ArrayList<>();
            for (App installed : Packages.all()) list.add(installed.applicationInfo(flags));
            return new android.content.pm.ParceledListSlice<>(list);
        }

        public java.util.List<String> getAllPackages() {
            java.util.List<String> names = new java.util.ArrayList<>();
            for (App installed : Packages.all()) names.add(installed.packageName());
            return names;
        }

        public boolean isPackageAvailable(String packageName, int userId) {
            return Packages.get(packageName) != null;
        }

        public int getPackageUid(String packageName, long flags, int userId) {
            App installed = Packages.get(packageName);
            return installed == null ? android.os.Process.INVALID_UID : installed.application.uid;
        }

        public int getTargetSdkVersion(String packageName) {
            App installed = Packages.get(packageName);
            return installed == null ? -1 : installed.application.targetSdkVersion;
        }

        public android.content.pm.ParceledListSlice<android.content.pm.ResolveInfo> queryIntentActivities(
                Intent intent, String resolvedType, long flags, int userId) {
            return new android.content.pm.ParceledListSlice<>(
                    Packages.query(Packages.Kind.ACTIVITY, intent, resolvedType, flags));
        }

        public android.content.pm.ResolveInfo resolveIntent(Intent intent, String resolvedType, long flags, int userId) {
            return Packages.resolve(Packages.Kind.ACTIVITY, intent, resolvedType, flags);
        }

        public android.content.pm.ParceledListSlice<android.content.pm.ResolveInfo> queryIntentReceivers(
                Intent intent, String resolvedType, long flags, int userId) {
            return new android.content.pm.ParceledListSlice<>(
                    Packages.query(Packages.Kind.RECEIVER, intent, resolvedType, flags));
        }

        public android.content.pm.ParceledListSlice<android.content.pm.ResolveInfo> queryIntentServices(
                Intent intent, String resolvedType, long flags, int userId) {
            return new android.content.pm.ParceledListSlice<>(
                    Packages.query(Packages.Kind.SERVICE, intent, resolvedType, flags));
        }

        public android.content.pm.ResolveInfo resolveService(Intent intent, String resolvedType, long flags, int userId) {
            return Packages.resolve(Packages.Kind.SERVICE, intent, resolvedType, flags);
        }

        public android.content.pm.ParceledListSlice<android.content.pm.ResolveInfo> queryIntentContentProviders(
                Intent intent, String resolvedType, long flags, int userId) {
            return new android.content.pm.ParceledListSlice<>(
                    Packages.query(Packages.Kind.PROVIDER, intent, resolvedType, flags));
        }

        /** The state set for a component of any installed app; Android refuses an unknown package. */
        public int getComponentEnabledSetting(ComponentName component, int userId) {
            if (Packages.get(component.getPackageName()) == null) {
                throw new IllegalArgumentException("Unknown component: " + component);
            }
            return EnabledSettings.get(component.getPackageName(), component.getClassName());
        }

        /**
         * Changes a component of the app itself and keeps it across starts; another app's components are
         * the system's to change. The app is not restarted, whatever the flags, as with DONT_KILL_APP.
         */
        public void setComponentEnabledSetting(ComponentName component, int newState, int flags, int userId,
                String callingPackage) {
            checkComponentChange(component, newState);
            EnabledSettings.set(component.getPackageName(), component.getClassName(), newState);
        }

        /** Refuses a change of another app's component, of a class the app does not declare, or to no state. */
        private void checkComponentChange(ComponentName component, int newState) {
            if (!component.getPackageName().equals(app.packageName())) {
                throw new SecurityException("Attempt to change component state; pid=" + android.os.Process.myPid()
                        + ", uid=" + app.application.uid + ", component=" + component);
            }
            Components declared = app.components();
            String name = component.getClassName();
            if (Components.find(declared.activities, name) == null && Components.find(declared.receivers, name) == null
                    && Components.find(declared.services, name) == null
                    && Components.find(declared.providers, name) == null) {
                throw new IllegalArgumentException("Component class " + name + " does not exist in "
                        + component.getPackageName());
            }
            if (newState < android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_DEFAULT
                    || newState > android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_DISABLED_UNTIL_USED) {
                throw new IllegalArgumentException("Invalid new component state: " + newState);
            }
        }

        /** Every entry is checked before any is written, so a list with a bad entry changes nothing. */
        public void setComponentEnabledSettings(
                java.util.List<android.content.pm.PackageManager.ComponentEnabledSetting> settings, int userId,
                String callingPackage) {
            for (android.content.pm.PackageManager.ComponentEnabledSetting setting : settings) {
                if (!setting.isComponent()) {
                    throw new IllegalArgumentException("An app is not turned off as a whole here: "
                            + setting.getPackageName());
                }
                checkComponentChange(setting.getComponentName(), setting.getEnabledState());
            }
            for (android.content.pm.PackageManager.ComponentEnabledSetting setting : settings) {
                EnabledSettings.set(setting.getPackageName(), setting.getClassName(), setting.getEnabledState());
            }
        }

        /** No app is turned off as a whole here. */
        public int getApplicationEnabledSetting(String packageName, int userId) {
            if (Packages.get(packageName) == null) throw new IllegalArgumentException("Unknown package: " + packageName);
            return android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_DEFAULT;
        }

        /**
         * Whether the app may ask to install others: when its manifest requests REQUEST_INSTALL_PACKAGES,
         * since a requested permission counts as granted here. Android answers only for the caller.
         */
        public boolean canRequestPackageInstalls(String packageName, int userId) {
            if (!packageName.equals(app.packageName())) {
                throw new SecurityException("Caller uid " + app.application.uid + " does not own package " + packageName);
            }
            return Permissions.requested(android.Manifest.permission.REQUEST_INSTALL_PACKAGES);
        }

        /** Installed from a file, as adb would: no installer, initiator or origin. */
        public android.content.pm.InstallSourceInfo getInstallSourceInfo(String packageName, int userId) {
            if (Packages.get(packageName) == null) return null;
            return new android.content.pm.InstallSourceInfo(null, null, null, null);
        }

        /** Every package with the uid, the caller first, as Android lists a shared uid's packages. */
        public String[] getPackagesForUid(int uid) {
            java.util.List<App> found = Packages.byUid(uid);
            if (found.isEmpty()) return null;
            String[] names = new String[found.size()];
            for (int i = 0; i < names.length; i++) names[i] = found.get(i).packageName();
            return names;
        }

        /** The caller for its own uid, else the first package with it. */
        public String getNameForUid(int uid) {
            java.util.List<App> found = Packages.byUid(uid);
            return found.isEmpty() ? null : found.get(0).packageName();
        }

        public boolean hasSystemFeature(String name, int version) {
            return Features.has(name, version);
        }

        public android.content.pm.ParceledListSlice<android.content.pm.FeatureInfo> getSystemAvailableFeatures() {
            return new android.content.pm.ParceledListSlice<>(Features.list());
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
