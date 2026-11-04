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
        graphics();
        redirect();
        broadcasts();
    }

    /**
     * Broadcasts inside the app: a receiver it registers, one of its manifest, an ordered broadcast
     * through receivers by priority to a result receiver, and an alarm whose PendingIntent is a
     * broadcast. An implicit broadcast reaches no manifest receiver, as on Android since 8.0.
     */
    private void broadcasts() {
        registerReceiver(new android.content.BroadcastReceiver() {
            @Override
            public void onReceive(android.content.Context context, android.content.Intent intent) {
                System.out.println("broadcast registered " + intent.getAction() + " main "
                        + (android.os.Looper.myLooper() == android.os.Looper.getMainLooper()));
            }
        }, new android.content.IntentFilter("com.example.compat.PING"), RECEIVER_NOT_EXPORTED);
        sendBroadcast(new android.content.Intent("com.example.compat.PING"));
        sendBroadcast(new android.content.Intent("com.example.compat.REMIND").setPackage(getPackageName())
                .putExtra("by", "package"));
        sendBroadcast(new android.content.Intent("com.example.compat.REMIND").putExtra("by", "implicit"));

        android.content.IntentFilter first = new android.content.IntentFilter("com.example.compat.ORDERED");
        first.setPriority(10);
        android.content.IntentFilter second = new android.content.IntentFilter("com.example.compat.ORDERED");
        second.setPriority(1);
        registerReceiver(new android.content.BroadcastReceiver() {
            @Override
            public void onReceive(android.content.Context context, android.content.Intent intent) {
                setResult(getResultCode() + 1, getResultData() + "b", null);
            }
        }, second, RECEIVER_NOT_EXPORTED);
        registerReceiver(new android.content.BroadcastReceiver() {
            @Override
            public void onReceive(android.content.Context context, android.content.Intent intent) {
                setResult(getResultCode() + 1, getResultData() + "a", null);
            }
        }, first, RECEIVER_NOT_EXPORTED);
        sendOrderedBroadcast(new android.content.Intent("com.example.compat.ORDERED"), null,
                new android.content.BroadcastReceiver() {
                    @Override
                    public void onReceive(android.content.Context context, android.content.Intent intent) {
                        System.out.println("broadcast ordered result " + getResultData() + " code " + getResultCode());
                    }
                }, null, 0, "", null);

        android.app.AlarmManager alarms = getSystemService(android.app.AlarmManager.class);
        android.content.Intent due = new android.content.Intent("com.example.compat.REMIND")
                .setPackage(getPackageName()).putExtra("by", "alarm");
        alarms.setExact(android.app.AlarmManager.ELAPSED_REALTIME, android.os.SystemClock.elapsedRealtime() - 1,
                android.app.PendingIntent.getBroadcast(this, 10, due, android.app.PendingIntent.FLAG_IMMUTABLE));
        alarms.setExact(android.app.AlarmManager.ELAPSED_REALTIME, android.os.SystemClock.elapsedRealtime() + 3_600_000,
                android.app.PendingIntent.getBroadcast(this, 11, new android.content.Intent(due).putExtra("by", "later alarm"),
                        android.app.PendingIntent.FLAG_IMMUTABLE));
    }

    /**
     * What GLSurfaceView and Mindustry's renderer do, offscreen: EGL10 finds an ES 2 config, makes a
     * context current on a pbuffer, and GLES20 clears it, compiles a shader and reads a pixel back.
     */
    private void graphics() {
        javax.microedition.khronos.egl.EGL10 egl =
                (javax.microedition.khronos.egl.EGL10) javax.microedition.khronos.egl.EGLContext.getEGL();
        javax.microedition.khronos.egl.EGLDisplay display = egl.eglGetDisplay(javax.microedition.khronos.egl.EGL10.EGL_DEFAULT_DISPLAY);
        int[] version = new int[2];
        boolean initialized = egl.eglInitialize(display, version);
        int[] count = new int[1];
        javax.microedition.khronos.egl.EGLConfig[] configs = new javax.microedition.khronos.egl.EGLConfig[1];
        int[] wanted = {javax.microedition.khronos.egl.EGL10.EGL_RED_SIZE, 8, javax.microedition.khronos.egl.EGL10.EGL_GREEN_SIZE, 8,
                javax.microedition.khronos.egl.EGL10.EGL_BLUE_SIZE, 8, javax.microedition.khronos.egl.EGL10.EGL_RENDERABLE_TYPE, 4,
                javax.microedition.khronos.egl.EGL10.EGL_SURFACE_TYPE, javax.microedition.khronos.egl.EGL10.EGL_PBUFFER_BIT,
                javax.microedition.khronos.egl.EGL10.EGL_NONE};
        egl.eglChooseConfig(display, wanted, configs, 1, count);
        if (!initialized || count[0] < 1) {
            System.out.println("graphics no EGL config, initialized " + initialized);
            return;
        }
        javax.microedition.khronos.egl.EGLContext context = egl.eglCreateContext(display, configs[0],
                javax.microedition.khronos.egl.EGL10.EGL_NO_CONTEXT, new int[] {0x3098, 2, javax.microedition.khronos.egl.EGL10.EGL_NONE});
        javax.microedition.khronos.egl.EGLSurface surface = egl.eglCreatePbufferSurface(display, configs[0],
                new int[] {javax.microedition.khronos.egl.EGL10.EGL_WIDTH, 4, javax.microedition.khronos.egl.EGL10.EGL_HEIGHT, 4,
                        javax.microedition.khronos.egl.EGL10.EGL_NONE});
        boolean current = egl.eglMakeCurrent(display, surface, surface, context);
        android.opengl.GLES20.glClearColor(1f, 0.5f, 0f, 1f);
        android.opengl.GLES20.glClear(android.opengl.GLES20.GL_COLOR_BUFFER_BIT);
        java.nio.ByteBuffer pixel = java.nio.ByteBuffer.allocateDirect(4);
        android.opengl.GLES20.glReadPixels(1, 1, 1, 1, android.opengl.GLES20.GL_RGBA, android.opengl.GLES20.GL_UNSIGNED_BYTE, pixel);
        int shader = android.opengl.GLES20.glCreateShader(android.opengl.GLES20.GL_VERTEX_SHADER);
        android.opengl.GLES20.glShaderSource(shader, "void main() { gl_Position = vec4(0.0); }");
        android.opengl.GLES20.glCompileShader(shader);
        int[] compiled = new int[1];
        android.opengl.GLES20.glGetShaderiv(shader, android.opengl.GLES20.GL_COMPILE_STATUS, compiled, 0);
        String refused;
        try {
            android.opengl.GLES20.glGenTextures(4, new int[2], 0);
            refused = "nothing";
        } catch (IllegalArgumentException e) {
            refused = "a short array";
        }
        System.out.println("graphics EGL " + version[0] + "." + version[1] + " current " + current
                + " GL " + (android.opengl.GLES20.glGetString(android.opengl.GLES20.GL_VERSION) != null)
                + " pixel " + String.format("%02x%02x%02x%02x", pixel.get(0), pixel.get(1), pixel.get(2), pixel.get(3))
                + " shader " + compiled[0] + " refused " + refused);
        egl.eglMakeCurrent(display, javax.microedition.khronos.egl.EGL10.EGL_NO_SURFACE,
                javax.microedition.khronos.egl.EGL10.EGL_NO_SURFACE, javax.microedition.khronos.egl.EGL10.EGL_NO_CONTEXT);
        egl.eglDestroySurface(display, surface);
        egl.eglDestroyContext(display, context);
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
