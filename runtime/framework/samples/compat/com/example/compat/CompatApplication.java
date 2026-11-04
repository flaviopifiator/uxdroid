package com.example.compat;

import android.app.Application;
import android.os.Binder;
import android.os.Process;

/** Prints what the corpus apps asked of the framework while their Application was created. */
public class CompatApplication extends Application {
    /** Set once onCreate starts: providers of the app are made before it. */
    static volatile boolean created;

    @Override
    public void onCreate() {
        created = true;
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
        providers();
        stores();
    }

    /** The app's own providers answer through ContentResolver; a disabled one has no authority. */
    private void providers() {
        for (String authority : new String[] {"provider", "remote", "disabled"}) {
            android.net.Uri uri = android.net.Uri.parse("content://com.example.compat." + authority + "/row");
            try (android.database.Cursor cursor = getContentResolver().query(uri, null, null, null, null)) {
                String answer = cursor != null && cursor.moveToFirst() ? cursor.getString(0) : "none";
                System.out.println("provider query " + authority + " " + answer);
            }
        }
        android.content.pm.ProviderInfo resolved = getPackageManager().resolveContentProvider(
                "com.example.compat.provider", 0);
        System.out.println("provider resolved " + (resolved == null ? null : resolved.name) + " meta-data "
                + (resolved == null ? "none" : resolved.metaData));
    }

    /**
     * What Aurora Store and microG asked on start: each manager exists and answers with an object. Each
     * check runs on its own, so one that fails does not hide the others.
     */
    private void stores() {
        check("installer", () -> {
            android.content.pm.PackageInstaller installer = getPackageManager().getPackageInstaller();
            System.out.println("stores installer sessions " + installer.getMySessions().size() + " all "
                    + installer.getAllSessions().size());
            try {
                installer.createSession(new android.content.pm.PackageInstaller.SessionParams(
                        android.content.pm.PackageInstaller.SessionParams.MODE_FULL_INSTALL));
                System.out.println("stores session created");
            } catch (java.io.IOException | UnsupportedOperationException e) {
                System.out.println("stores session refused " + e.getMessage());
            }
        });
        check("power", () -> {
            android.os.PowerManager power = getSystemService(android.os.PowerManager.class);
            System.out.println("stores power interactive " + power.isInteractive() + " save " + power.isPowerSaveMode()
                    + " idle " + power.isDeviceIdleMode());
            System.out.println("stores power ignoring battery optimizations "
                    + power.isIgnoringBatteryOptimizations(getPackageName()));
        });
        check("domains", () -> {
            android.content.pm.verify.domain.DomainVerificationManager domains =
                    getSystemService(android.content.pm.verify.domain.DomainVerificationManager.class);
            try {
                android.content.pm.verify.domain.DomainVerificationUserState state =
                        domains.getDomainVerificationUserState(getPackageName());
                System.out.println("stores domains allowed " + state.isLinkHandlingAllowed() + " hosts "
                        + state.getHostToStateMap());
            } catch (android.content.pm.PackageManager.NameNotFoundException e) {
                throw new IllegalStateException(e);
            }
            try {
                domains.getDomainVerificationUserState("com.example.absent");
                System.out.println("stores domains of another package found");
            } catch (android.content.pm.PackageManager.NameNotFoundException e) {
                System.out.println("stores domains of another package not found");
            }
        });
        check("jobs", () -> {
            android.app.job.JobScheduler jobs = getSystemService(android.app.job.JobScheduler.class);
            System.out.println("stores jobs pending " + jobs.getAllPendingJobs().size() + " job " + jobs.getPendingJob(1));
        });
        check("accounts", () -> {
            android.accounts.AccountManager accounts = android.accounts.AccountManager.get(this);
            System.out.println("stores accounts " + accounts.getAccounts().length + " google "
                    + accounts.getAccountsByType("com.google").length + " authenticators "
                    + accounts.getAuthenticatorTypes().length);
        });
    }

    private static void check(String name, Runnable check) {
        try {
            check.run();
        } catch (RuntimeException e) {
            System.out.println("stores " + name + " failed " + e);
        }
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
        // An alarm clock that went off is no longer the next one.
        alarms.setAlarmClock(new android.app.AlarmManager.AlarmClockInfo(System.currentTimeMillis() - 1, null),
                android.app.PendingIntent.getBroadcast(this, 12, new android.content.Intent("com.example.compat.REMIND")
                        .setPackage(getPackageName()).putExtra("by", "alarm clock"), android.app.PendingIntent.FLAG_IMMUTABLE));
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
        // GLES31 and GLES32, on the same context: a program pipeline and a texture's border color.
        int[] pipelines = new int[1];
        android.opengl.GLES31.glGenProgramPipelines(1, pipelines, 0);
        android.opengl.GLES31.glBindProgramPipeline(pipelines[0]);
        String pipelineLog = android.opengl.GLES31.glGetProgramPipelineInfoLog(pipelines[0]);
        int[] texture = new int[1];
        android.opengl.GLES20.glGenTextures(1, texture, 0);
        android.opengl.GLES20.glBindTexture(android.opengl.GLES20.GL_TEXTURE_2D, texture[0]);
        android.opengl.GLES32.glTexParameterIiv(android.opengl.GLES20.GL_TEXTURE_2D,
                android.opengl.GLES32.GL_TEXTURE_BORDER_COLOR, new int[] {1, 2, 3, 4}, 0);
        int[] border = new int[4];
        android.opengl.GLES32.glGetTexParameterIiv(android.opengl.GLES20.GL_TEXTURE_2D,
                android.opengl.GLES32.GL_TEXTURE_BORDER_COLOR, border, 0);
        String shortBorder;
        try {
            android.opengl.GLES32.glGetTexParameterIiv(android.opengl.GLES20.GL_TEXTURE_2D,
                    android.opengl.GLES32.GL_TEXTURE_BORDER_COLOR, new int[2], 0);
            shortBorder = "accepted";
        } catch (IllegalArgumentException e) {
            shortBorder = "refused";
        }
        System.out.println("graphics31 pipeline " + android.opengl.GLES31.glIsProgramPipeline(pipelines[0])
                + " log " + (pipelineLog != null) + " border " + java.util.Arrays.toString(border)
                + " short border " + shortBorder);
        // An indexed query of GL_COLOR_WRITEMASK writes four booleans, and client indices hold their
        // count of their type: arrays and buffers shorter than that are refused before GL runs.
        boolean[] mask = new boolean[4];
        android.opengl.GLES32.glGetBooleani_v(android.opengl.GLES20.GL_COLOR_WRITEMASK, 0, mask, 0);
        String shortMask;
        try {
            android.opengl.GLES32.glGetBooleani_v(android.opengl.GLES20.GL_COLOR_WRITEMASK, 0, new boolean[1], 0);
            shortMask = "accepted";
        } catch (IllegalArgumentException e) {
            shortMask = "refused";
        }
        java.nio.ShortBuffer six = java.nio.ByteBuffer.allocateDirect(12)
                .order(java.nio.ByteOrder.nativeOrder()).asShortBuffer();
        java.nio.ShortBuffer one = java.nio.ByteBuffer.allocateDirect(2)
                .order(java.nio.ByteOrder.nativeOrder()).asShortBuffer();
        android.opengl.GLES32.glDrawElementsBaseVertex(android.opengl.GLES20.GL_TRIANGLES, 6,
                android.opengl.GLES20.GL_UNSIGNED_SHORT, six, 0);
        String shortIndices;
        try {
            android.opengl.GLES32.glDrawElementsBaseVertex(android.opengl.GLES20.GL_TRIANGLES, 6,
                    android.opengl.GLES20.GL_UNSIGNED_SHORT, one, 0);
            shortIndices = "accepted";
        } catch (IllegalArgumentException e) {
            shortIndices = "refused";
        }
        System.out.println("graphics32 mask " + java.util.Arrays.toString(mask) + " short mask " + shortMask
                + " short indices " + shortIndices);
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

        // A list over the limit is refused whole: what was there stays.
        java.util.List<android.content.pm.ShortcutInfo> many = new java.util.ArrayList<>();
        for (int i = 0; i < shortcuts.getMaxShortcutCountPerActivity(); i++) {
            many.add(new android.content.pm.ShortcutInfo.Builder(this, "s" + i).setShortLabel("S" + i)
                    .setIntent(new android.content.Intent("com.example.compat.SECOND")).build());
        }
        String refused = "accepted";
        try {
            shortcuts.addDynamicShortcuts(many);
        } catch (IllegalArgumentException e) {
            refused = "refused";
        }
        System.out.println("shortcuts over the limit " + refused + " kept " + shortcuts.getDynamicShortcuts().size());

        // Apps choose their layout and features by what the device has: a desktop with a touchscreen,
        // sound, a microphone and OpenGL ES, and no camera, telephony or WebView.
        android.content.pm.PackageManager pm = getPackageManager();
        StringBuilder features = new StringBuilder("features");
        for (String feature : new String[] {"android.hardware.type.pc", "android.hardware.touchscreen",
                "android.hardware.audio.output", "android.hardware.microphone", "android.hardware.camera",
                "android.hardware.telephony", "android.software.webview"}) {
            features.append(' ').append(feature.substring(feature.lastIndexOf('.') + 1)).append(' ')
                    .append(pm.hasSystemFeature(feature));
        }
        features.append(" listed ").append(pm.getSystemAvailableFeatures().length > 0);
        System.out.println(features);
        // AnkiDroid's text fields ask for spell checkers; a desktop has none, and says so.
        android.view.textservice.TextServicesManager text =
                getSystemService(android.view.textservice.TextServicesManager.class);
        System.out.println("textservices " + (text != null) + " spell checker "
                + (text != null && text.isSpellCheckerEnabled()));

        // Mindustry's graphics setup asks for its game mode.
        android.app.GameManager games = getSystemService(android.app.GameManager.class);
        System.out.println("game mode " + (games == null ? "none" : games.getGameMode()));
    }

    private static android.net.ConnectivityManager.NetworkCallback callback(String name) {
        return new android.net.ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(android.net.Network network) {
                System.out.println("network callback " + name + " available");
            }

            @Override
            public void onCapabilitiesChanged(android.net.Network network, android.net.NetworkCapabilities caps) {
                System.out.println("network callback " + name + " internet "
                        + caps.hasCapability(android.net.NetworkCapabilities.NET_CAPABILITY_INTERNET));
            }
        };
    }

    /** Glide, in Aegis, holds the ConnectivityManager and watches the default network. */
    private void network() {
        android.net.ConnectivityManager connectivity = getSystemService(android.net.ConnectivityManager.class);
        if (connectivity == null) {
            System.out.println("network no ConnectivityManager");
            return;
        }
        // Watching the network, as Glide and WorkManager do: each callback hears of the current
        // network once, with its capabilities, on ConnectivityManager's own thread, as on Android.
        connectivity.registerDefaultNetworkCallback(callback("default"));
        connectivity.registerNetworkCallback(new android.net.NetworkRequest.Builder()
                .addCapability(android.net.NetworkCapabilities.NET_CAPABILITY_INTERNET).build(), callback("internet"));
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
