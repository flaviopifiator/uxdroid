package dev.uxdroid.system;

import android.app.AlarmManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationChannelGroup;
import android.app.SyncNotedAppOp;
import android.content.pm.ParceledListSlice;
import android.content.pm.ShortcutInfo;
import android.os.SystemClock;
import android.service.notification.StatusBarNotification;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * System services the F-Droid corpus (runtime/framework/corpus_smoke.sh) needed beyond the ones the
 * launcher registers itself, each with the behavior those apps rely on rather than defaults. An app
 * usually stops on the null manager a missing service gives it.
 */
final class AppServices {
    private AppServices() {}

    static void register(Launcher.App app) {
        Services.register("connectivity", android.net.IConnectivityManager.class, new NetworkServices.Connectivity());
        Services.register("netstats", android.net.INetworkStatsService.class, new NetworkServices.Stats());
        Services.register("notification", android.app.INotificationManager.class, new Notifications(app));
        Services.register("alarm", android.app.IAlarmManager.class, new Alarms());
        Services.register("appops", com.android.internal.app.IAppOpsService.class, new AppOps(app));
        Services.register("shortcut", android.content.pm.IShortcutService.class, new Shortcuts(app));
        Services.register("game", android.app.IGameManagerService.class, new Games(app));
    }

    /**
     * The handler of INotificationManager: the app's channels, channel groups and posted notifications,
     * kept for the process's life. Notifications are enabled, as an app on a desktop has no permission
     * dialog, and each posted one is printed; showing it on the desktop, through the freedesktop
     * notification service on D-Bus, is not done yet.
     */
    public static final class Notifications {
        private final Launcher.App app;
        private final Map<String, NotificationChannel> channels = new LinkedHashMap<>();
        private final Map<String, NotificationChannelGroup> groups = new LinkedHashMap<>();
        private final Map<String, StatusBarNotification> posted = new LinkedHashMap<>();

        Notifications(Launcher.App app) {
            this.app = app;
        }

        private static String key(String tag, int id) {
            return (tag == null ? "" : tag) + "|" + id;
        }

        public boolean areNotificationsEnabled(String pkg) { return true; }
        public boolean areNotificationsEnabledForPackage(String pkg, int uid) { return true; }
        public boolean isPermissionFixed(String pkg, int userId) { return true; }

        public synchronized void createNotificationChannels(String pkg, ParceledListSlice list) {
            for (Object channel : list.getList()) {
                NotificationChannel created = (NotificationChannel) channel;
                channels.put(created.getId(), created);
            }
        }

        public synchronized void createNotificationChannelGroups(String pkg, ParceledListSlice list) {
            for (Object group : list.getList()) {
                NotificationChannelGroup created = (NotificationChannelGroup) group;
                groups.put(created.getId(), created);
            }
        }

        public synchronized NotificationChannel getNotificationChannel(String callingPkg, int userId, String pkg,
                String channelId) {
            return channels.get(channelId);
        }

        public synchronized ParceledListSlice getNotificationChannels(String callingPkg, String targetPkg, int userId) {
            return new ParceledListSlice<>(new ArrayList<>(channels.values()));
        }

        public synchronized void deleteNotificationChannel(String pkg, String channelId) {
            channels.remove(channelId);
        }

        public synchronized NotificationChannelGroup getNotificationChannelGroup(String pkg, String groupId) {
            return groups.get(groupId);
        }

        public synchronized ParceledListSlice getNotificationChannelGroups(String pkg) {
            return new ParceledListSlice<>(new ArrayList<>(groups.values()));
        }

        public synchronized void deleteNotificationChannelGroup(String pkg, String groupId) {
            groups.remove(groupId);
        }

        public synchronized void enqueueNotificationWithTag(String pkg, String opPkg, String tag, int id,
                Notification notification, int userId) {
            posted.put(key(tag, id), new StatusBarNotification(pkg, opPkg, id, tag, app.application.uid,
                    android.os.Process.myPid(), notification, android.os.UserHandle.of(userId), null,
                    System.currentTimeMillis()));
            CharSequence title = notification.extras.getCharSequence(Notification.EXTRA_TITLE);
            System.err.println("uxdroid: notification " + pkg + " " + id + ": " + title);
        }

        public synchronized void cancelNotificationWithTag(String pkg, String opPkg, String tag, int id, int userId) {
            posted.remove(key(tag, id));
        }

        public synchronized void cancelAllNotifications(String pkg, int userId) {
            posted.clear();
        }

        public synchronized ParceledListSlice getAppActiveNotifications(String pkg, int userId) {
            return new ParceledListSlice<>(new ArrayList<>(posted.values()));
        }

        public synchronized StatusBarNotification[] getActiveNotifications(String callingPkg) {
            return posted.values().toArray(new StatusBarNotification[0]);
        }
    }

    /**
     * The handler of IAlarmManager. An alarm with a listener, as AlarmManager.set with an
     * OnAlarmListener makes, fires on time from a timer thread and the listener runs on its own handler;
     * repeating ones fire again. An alarm with a PendingIntent sends it at its time, which for a
     * broadcast reaches the app's receivers ({@link Broadcasts}).
     */
    public static final class Alarms {
        private final java.util.concurrent.ScheduledExecutorService timer =
                java.util.concurrent.Executors.newSingleThreadScheduledExecutor(task -> {
                    Thread thread = new Thread(task, "uxdroid-alarms");
                    thread.setDaemon(true);
                    return thread;
                });
        private final List<Alarm> alarms = new ArrayList<>();

        private static final class Alarm {
            final android.app.PendingIntent operation;
            final android.app.IAlarmListener listener;
            final AlarmManager.AlarmClockInfo clock;
            java.util.concurrent.ScheduledFuture<?> future;

            Alarm(android.app.PendingIntent operation, android.app.IAlarmListener listener,
                    AlarmManager.AlarmClockInfo clock) {
                this.operation = operation;
                this.listener = listener;
                this.clock = clock;
            }
        }

        /** What a listener tells when its alarm is handled; nothing waits for it here. */
        public static final class Complete {
            public void alarmComplete(android.os.IBinder who) {}
        }

        private static final android.app.IAlarmCompleteListener COMPLETE =
                Services.local(android.app.IAlarmCompleteListener.class, new Complete());

        /** Milliseconds from now until a trigger time of the given type, at least 0. */
        static long delay(int type, long triggerAt) {
            boolean wallClock = type == AlarmManager.RTC || type == AlarmManager.RTC_WAKEUP;
            long now = wallClock ? System.currentTimeMillis() : SystemClock.elapsedRealtime();
            return Math.max(0, triggerAt - now);
        }

        public synchronized void set(String callingPackage, int type, long triggerAt, long windowLength,
                long interval, int flags, android.app.PendingIntent operation, android.app.IAlarmListener listener,
                String listenerTag, android.os.WorkSource workSource, AlarmManager.AlarmClockInfo clock) {
            remove(operation, listener);
            Alarm alarm = new Alarm(operation, listener, clock);
            alarms.add(alarm);
            Runnable fire = () -> {
                // Gone before it is handled, so what it starts never sees it as the next alarm.
                if (interval <= 0) {
                    synchronized (this) {
                        alarms.remove(alarm);
                    }
                }
                try {
                    if (listener != null) {
                        listener.doAlarm(COMPLETE);
                    } else if (operation != null) {
                        // As AlarmManagerService sends an alarm's operation: its broadcast, activity or service.
                        IntentSenders.send(operation.getTarget(), null);
                    }
                } catch (android.os.RemoteException | RuntimeException e) {
                    System.err.println("uxdroid: an alarm of " + callingPackage + " failed: " + e);
                }
            };
            alarm.future = interval > 0
                    ? timer.scheduleAtFixedRate(fire, delay(type, triggerAt), interval,
                            java.util.concurrent.TimeUnit.MILLISECONDS)
                    : timer.schedule(fire, delay(type, triggerAt), java.util.concurrent.TimeUnit.MILLISECONDS);
        }

        public synchronized void remove(android.app.PendingIntent operation, android.app.IAlarmListener listener) {
            alarms.removeIf(alarm -> {
                boolean same = (operation != null && operation.equals(alarm.operation))
                        || (listener != null && alarm.listener != null && listener.asBinder() == alarm.listener.asBinder());
                if (same && alarm.future != null) alarm.future.cancel(false);
                return same;
            });
        }

        public synchronized void removeAll(String callingPackage) {
            for (Alarm alarm : alarms) {
                if (alarm.future != null) alarm.future.cancel(false);
            }
            alarms.clear();
        }

        public synchronized AlarmManager.AlarmClockInfo getNextAlarmClock(int userId) {
            AlarmManager.AlarmClockInfo next = null;
            for (Alarm alarm : alarms) {
                if (alarm.clock != null && (next == null || alarm.clock.getTriggerTime() < next.getTriggerTime())) {
                    next = alarm.clock;
                }
            }
            return next;
        }

        public boolean canScheduleExactAlarms(String packageName) { return true; }
        public boolean hasScheduleExactAlarm(String packageName, int userId) { return true; }
        public long getNextWakeFromIdleTime() { return Long.MAX_VALUE; }
    }

    /**
     * The handler of IAppOpsService. A desktop has no permission dialog, so what the app asks for is
     * allowed, as its requested permissions count as granted (Permissions); an operation named for
     * another package than the app's is refused, as Android refuses a package its uid does not own.
     */
    public static final class AppOps {
        private final Launcher.App app;

        AppOps(Launcher.App app) {
            this.app = app;
        }

        private int mode(int uid, String packageName) {
            boolean own = packageName == null || packageName.equals(app.application.packageName);
            return own ? android.app.AppOpsManager.MODE_ALLOWED : android.app.AppOpsManager.MODE_ERRORED;
        }

        public int checkOperation(int code, int uid, String packageName) { return mode(uid, packageName); }

        public int checkOperationForDevice(int code, int uid, String packageName, String deviceId, int virtualDeviceId) {
            return mode(uid, packageName);
        }

        public int checkOperationRaw(int code, int uid, String packageName, String attributionTag) {
            return mode(uid, packageName);
        }

        public int checkOperationRawForDevice(int code, int uid, String packageName, String attributionTag,
                int virtualDeviceId) {
            return mode(uid, packageName);
        }

        public int checkAudioOperation(int code, int usage, int uid, String packageName) { return mode(uid, packageName); }
        public int checkPackage(int uid, String packageName) { return mode(uid, packageName); }

        public SyncNotedAppOp noteOperation(int code, int uid, String packageName, String attributionTag,
                boolean shouldCollectAsyncNotedOp, String message, boolean shouldCollectMessage) {
            return new SyncNotedAppOp(mode(uid, packageName), code, attributionTag, packageName);
        }

        public SyncNotedAppOp noteOperationForDevice(int code, int uid, String packageName, String attributionTag,
                int virtualDeviceId, boolean shouldCollectAsyncNotedOp, String message, boolean shouldCollectMessage) {
            return new SyncNotedAppOp(mode(uid, packageName), code, attributionTag, packageName);
        }

        public SyncNotedAppOp startOperation(android.os.IBinder clientId, int code, int uid, String packageName,
                String attributionTag, boolean startIfModeDefault, boolean shouldCollectAsyncNotedOp, String message,
                boolean shouldCollectMessage, int attributionFlags, int attributionChainId) {
            return new SyncNotedAppOp(mode(uid, packageName), code, attributionTag, packageName);
        }

        public void finishOperation(android.os.IBinder clientId, int code, int uid, String packageName,
                String attributionTag) {}

        public boolean shouldCollectNotes(int opCode) { return false; }

        public List<android.app.AsyncNotedAppOp> extractAsyncOps(String packageName) { return new ArrayList<>(); }
    }

    /**
     * The handler of IShortcutService: the app's dynamic shortcuts, kept for the process's life, within
     * Android's limit per activity. A desktop launcher cannot pin them, so pinning is not supported.
     */
    public static final class Shortcuts {
        /** Android's default of max_shortcuts. */
        static final int MAX_PER_ACTIVITY = 15;
        private final Launcher.App app;
        private final Map<String, ShortcutInfo> dynamic = new LinkedHashMap<>();

        Shortcuts(Launcher.App app) {
            this.app = app;
        }

        /** Adds or replaces the list whole, or refuses it whole when it would pass the limit. */
        private boolean add(ParceledListSlice list, boolean replace) {
            Map<String, ShortcutInfo> next = replace ? new LinkedHashMap<>() : new LinkedHashMap<>(dynamic);
            for (Object shortcut : list.getList()) {
                ShortcutInfo info = (ShortcutInfo) shortcut;
                next.put(info.getId(), info);
                if (next.size() > MAX_PER_ACTIVITY) {
                    throw new IllegalArgumentException("Max number of dynamic shortcuts exceeded");
                }
            }
            for (ShortcutInfo info : next.values()) info.addFlags(ShortcutInfo.FLAG_DYNAMIC);
            dynamic.clear();
            dynamic.putAll(next);
            return true;
        }

        public synchronized boolean setDynamicShortcuts(String packageName, ParceledListSlice list, int userId) {
            return add(list, true);
        }

        public synchronized boolean addDynamicShortcuts(String packageName, ParceledListSlice list, int userId) {
            return add(list, false);
        }

        /** As ShortcutService does, a push over the limit drops the oldest dynamic shortcut. */
        public synchronized void pushDynamicShortcut(String packageName, ShortcutInfo shortcut, int userId) {
            shortcut.addFlags(ShortcutInfo.FLAG_DYNAMIC);
            dynamic.remove(shortcut.getId());
            if (dynamic.size() >= MAX_PER_ACTIVITY) dynamic.remove(dynamic.keySet().iterator().next());
            dynamic.put(shortcut.getId(), shortcut);
        }

        public synchronized void removeDynamicShortcuts(String packageName, List<String> ids, int userId) {
            for (String id : ids) dynamic.remove(id);
        }

        public synchronized void removeAllDynamicShortcuts(String packageName, int userId) {
            dynamic.clear();
        }

        public synchronized void removeLongLivedShortcuts(String packageName, List<String> ids, int userId) {
            for (String id : ids) dynamic.remove(id);
        }

        /** Only dynamic shortcuts exist here: manifest, pinned and cached ones are none. */
        public synchronized ParceledListSlice getShortcuts(String packageName, int matchFlags, int userId) {
            List<ShortcutInfo> found = (matchFlags & android.content.pm.ShortcutManager.FLAG_MATCH_DYNAMIC) != 0
                    ? new ArrayList<>(dynamic.values()) : new ArrayList<>();
            return new ParceledListSlice<>(found);
        }

        public boolean updateShortcuts(String packageName, ParceledListSlice list, int userId) {
            synchronized (this) {
                for (Object shortcut : list.getList()) {
                    ShortcutInfo info = (ShortcutInfo) shortcut;
                    if (dynamic.containsKey(info.getId())) {
                        info.addFlags(ShortcutInfo.FLAG_DYNAMIC);
                        dynamic.put(info.getId(), info);
                    }
                }
            }
            return true;
        }

        public int getMaxShortcutCountPerActivity(String packageName, int userId) { return MAX_PER_ACTIVITY; }

        /** No rate limit, as for an app in the foreground. */
        public int getRemainingCallCount(String packageName, int userId) { return MAX_PER_ACTIVITY; }
        public long getRateLimitResetTime(String packageName, int userId) { return 0; }

        /** Android's default icon size in dp, at the display's density. */
        public int getIconMaxDimensions(String packageName, int userId) {
            return 96 * DisplayService.DENSITY / android.util.DisplayMetrics.DENSITY_DEFAULT;
        }

        public void reportShortcutUsed(String packageName, String shortcutId, int userId) {}
        public boolean isRequestPinItemSupported(int userId, int requestType) { return false; }
    }

    /**
     * The handler of IGameManagerService: a game, by its manifest's category, plays in the standard
     * mode, the one Android gives a game no one configured; other apps have no game mode.
     */
    public static final class Games {
        private final Launcher.App app;

        Games(Launcher.App app) {
            this.app = app;
        }

        private boolean game(String packageName) {
            android.content.pm.ApplicationInfo info = app.application;
            return packageName != null && packageName.equals(info.packageName)
                    && (info.category == android.content.pm.ApplicationInfo.CATEGORY_GAME
                            || (info.flags & android.content.pm.ApplicationInfo.FLAG_IS_GAME) != 0);
        }

        public int getGameMode(String packageName, int userId) {
            return game(packageName) ? android.app.GameManager.GAME_MODE_STANDARD
                    : android.app.GameManager.GAME_MODE_UNSUPPORTED;
        }

        public int[] getAvailableGameModes(String packageName, int userId) {
            return game(packageName) ? new int[] {android.app.GameManager.GAME_MODE_STANDARD} : new int[0];
        }

        public boolean isAngleEnabled(String packageName, int userId) { return false; }
        public void setGameState(String packageName, android.app.GameState state, int userId) {}
    }
}
