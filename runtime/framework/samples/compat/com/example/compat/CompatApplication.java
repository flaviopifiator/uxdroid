package com.example.compat;

import android.app.Application;
import android.os.Binder;
import android.os.Process;

/** Prints what the corpus apps asked of the framework while their Application was created. */
public class CompatApplication extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        // WebView and TrafficStats refuse the system uid; an app has one of its own.
        System.out.println("identity myUid " + Process.myUid() + " callingUid " + Binder.getCallingUid()
                + " applicationInfo.uid " + getApplicationInfo().uid + " userId " + Process.myUserHandle()
                + " isApplicationUid " + Process.isApplicationUid(Process.myUid()));
        database();
        network();
        services();
        device();
        redirect();
    }

    /**
     * AnkiDroid's Rust backend points stderr at a pipe and writes what it reads to the log, which must
     * not come back into the pipe; the log and System.err still reach the console.
     */
    private void redirect() {
        try {
            java.io.FileDescriptor[] pipe = android.system.Os.pipe();
            android.system.Os.dup2(pipe[1], 2);
            Thread reader = new Thread(() -> {
                try (java.io.BufferedReader lines = new java.io.BufferedReader(
                        new java.io.InputStreamReader(new java.io.FileInputStream(pipe[0])))) {
                    for (String line = lines.readLine(); line != null; line = lines.readLine()) {
                        android.util.Log.i("compat", "STDERR: " + line);
                    }
                } catch (java.io.IOException e) {
                    android.util.Log.e("compat", "stderr pipe", e);
                }
            });
            reader.setDaemon(true);
            reader.start();
        } catch (android.system.ErrnoException e) {
            throw new IllegalStateException(e);
        }
        android.util.Log.i("compat", "log after redirect");
        System.err.println("err after redirect");
    }

    /** What the app learns of the device: not a Robolectric test, and the GL classes of the platform. */
    private void device() {
        System.out.println("device fingerprint " + android.os.Build.FINGERPRINT + " model " + android.os.Build.MODEL
                + " tags " + android.os.Build.TAGS);
        String found;
        try {
            found = Class.forName("javax.microedition.khronos.opengles.GL10").getName() + " "
                    + Class.forName("javax.microedition.khronos.egl.EGLContext").getName();
        } catch (ClassNotFoundException e) {
            found = "missing " + e.getMessage();
        }
        System.out.println("khronos " + found);
    }

    /** The services the corpus apps stopped on when they were missing. */
    private void services() {
        // AnkiDroid creates its channels in its Application, then posts with them.
        android.app.NotificationManager notifications = getSystemService(android.app.NotificationManager.class);
        notifications.createNotificationChannel(new android.app.NotificationChannel("general", "General",
                android.app.NotificationManager.IMPORTANCE_DEFAULT));
        notifications.notify(7, new android.app.Notification.Builder(this, "general")
                .setSmallIcon(android.R.drawable.ic_dialog_info).setContentTitle("Due cards").build());
        System.out.println("notification channel " + notifications.getNotificationChannel("general").getName()
                + " enabled " + notifications.areNotificationsEnabled()
                + " active " + notifications.getActiveNotifications().length);
        notifications.cancel(7);
        System.out.println("notification cancelled active " + notifications.getActiveNotifications().length);

        // Loop Habit Tracker schedules its reminders. An alarm already due fires at once.
        android.app.AlarmManager alarms = getSystemService(android.app.AlarmManager.class);
        alarms.set(android.app.AlarmManager.ELAPSED_REALTIME, android.os.SystemClock.elapsedRealtime() - 1,
                "compat", () -> System.out.println("alarm fired on main thread "
                        + (android.os.Looper.myLooper() == android.os.Looper.getMainLooper())), null);
        android.app.AlarmManager.AlarmClockInfo clock = new android.app.AlarmManager.AlarmClockInfo(
                System.currentTimeMillis() + 3_600_000, null);
        alarms.setAlarmClock(clock, null);
        System.out.println("alarm next clock " + (alarms.getNextAlarmClock() != null
                && alarms.getNextAlarmClock().getTriggerTime() == clock.getTriggerTime())
                + " exact " + alarms.canScheduleExactAlarms());

        // Markor checks an operation; on a desktop what the app asks for is allowed.
        android.app.AppOpsManager ops = getSystemService(android.app.AppOpsManager.class);
        System.out.println("appops note " + ops.noteOpNoThrow(android.app.AppOpsManager.OPSTR_READ_EXTERNAL_STORAGE,
                android.os.Process.myUid(), getPackageName())
                + " check " + ops.unsafeCheckOpNoThrow(android.app.AppOpsManager.OPSTR_READ_EXTERNAL_STORAGE,
                android.os.Process.myUid(), getPackageName()));

        // Aegis publishes dynamic shortcuts.
        android.content.pm.ShortcutManager shortcuts = getSystemService(android.content.pm.ShortcutManager.class);
        shortcuts.setDynamicShortcuts(java.util.List.of(new android.content.pm.ShortcutInfo.Builder(this, "scan")
                .setShortLabel("Scan").setIntent(new android.content.Intent("com.example.compat.SECOND")).build()));
        System.out.println("shortcuts dynamic " + shortcuts.getDynamicShortcuts().size()
                + " " + shortcuts.getDynamicShortcuts().get(0).getId()
                + " max " + shortcuts.getMaxShortcutCountPerActivity()
                + " pin " + shortcuts.isRequestPinShortcutSupported());

        // Mindustry's graphics setup asks for its game mode.
        android.app.GameManager games = getSystemService(android.app.GameManager.class);
        System.out.println("game mode " + (games == null ? "none" : games.getGameMode()));
    }

    /** Glide, in Aegis, holds the ConnectivityManager and watches the default network. */
    private void network() {
        android.net.ConnectivityManager connectivity = getSystemService(android.net.ConnectivityManager.class);
        if (connectivity == null) {
            System.out.println("network no ConnectivityManager");
            return;
        }
        android.net.Network active = connectivity.getActiveNetwork();
        android.net.NetworkCapabilities capabilities = connectivity.getNetworkCapabilities(active);
        android.net.NetworkInfo info = connectivity.getActiveNetworkInfo();
        connectivity.registerDefaultNetworkCallback(new android.net.ConnectivityManager.NetworkCallback());
        System.out.println("network active " + (active != null)
                + " internet " + (capabilities != null
                        && capabilities.hasCapability(android.net.NetworkCapabilities.NET_CAPABILITY_INTERNET))
                + " connected " + (info != null && info.isConnected())
                + " total received " + (android.net.TrafficStats.getTotalRxBytes() >= 0)
                + " own received " + android.net.TrafficStats.getUidRxBytes(android.os.Process.myUid()));
    }

    /** Loop Habit Tracker opens its database while its Application is created. */
    private void database() {
        android.database.sqlite.SQLiteOpenHelper helper =
                new android.database.sqlite.SQLiteOpenHelper(this, "compat.db", null, 1) {
            @Override
            public void onCreate(android.database.sqlite.SQLiteDatabase db) {
                db.execSQL("CREATE TABLE habit (name TEXT)");
            }

            @Override
            public void onUpgrade(android.database.sqlite.SQLiteDatabase db, int from, int to) {}
        };
        android.database.sqlite.SQLiteDatabase db = helper.getWritableDatabase();
        db.execSQL("INSERT INTO habit VALUES ('read')");
        try (android.database.Cursor rows = db.rawQuery("SELECT count(*) FROM habit", null)) {
            rows.moveToFirst();
            System.out.println("database " + db.getPath() + " rows " + rows.getInt(0) + " page size " + db.getPageSize());
        }
        helper.close();
    }
}
