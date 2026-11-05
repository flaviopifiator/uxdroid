package dev.uxdroid.system;

import android.app.Activity;
import android.app.IApplicationThread;
import android.app.ResultInfo;
import android.app.servertransaction.ActivityResultItem;
import android.app.servertransaction.ClientTransaction;
import android.app.servertransaction.DestroyActivityItem;
import android.app.servertransaction.LaunchActivityItem;
import android.app.servertransaction.NewIntentItem;
import android.app.servertransaction.PauseActivityItem;
import android.app.servertransaction.ResumeActivityItem;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageParser;
import android.content.res.Configuration;
import android.os.Binder;
import android.os.IBinder;
import android.os.RemoteException;
import android.window.ActivityWindowInfo;
import com.android.internal.content.ReferrerIntent;
import java.util.ArrayList;
import java.util.List;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * The app's one task: its activities from bottom to top, and what a new intent does to them, decided as
 * Android's ActivityTaskManager decides it from the activity's launch mode and the intent's flags. Every
 * change reaches the app as the framework's own client transactions. Runs on the main thread only.
 *
 * <p>Each activity has its own window, as in desktop windowing: an activity covered by a newer one is
 * paused, not stopped, and its window stays.
 */
final class ActivityStack {
    /** The stack of the running app, once it is attached. */
    static ActivityStack current;

    static final class Entry {
        final IBinder token = new Binder();
        final ActivityInfo info;
        boolean resumed;
        /** Who started it for a result, and asked with what; null when nobody waits for one. */
        Result result;

        Entry(ActivityInfo info) {
            this.info = info;
        }

        String component() {
            return new ComponentName(info.packageName, info.name).flattenToShortString();
        }
    }

    /** What startActivityForResult asked: the activity that waits, its embedded id and request code. */
    static final class Result {
        final IBinder to;
        final String who;
        final int requestCode;

        Result(IBinder to, String who, int requestCode) {
            this.to = to;
            this.who = who;
            this.requestCode = requestCode;
        }

        /** The result of Instrumentation's start, or null when it asked for none. */
        static Result of(IBinder to, String who, int requestCode) {
            return to != null && requestCode >= 0 ? new Result(to, who, requestCode) : null;
        }
    }

    /** Refused by the app's own rules, with the protocol's code. */
    static final class Refusal extends Exception {
        final String code;

        Refusal(String code, String message) {
            super(message);
            this.code = code;
        }
    }

    private final Launcher.App app;
    private final IApplicationThread thread;
    private final boolean resume;
    private final ArrayList<Entry> entries = new ArrayList<>();

    /** @param resume false for --once, whose activity is created and never shown */
    ActivityStack(Launcher.App app, IApplicationThread thread, boolean resume) {
        this.app = app;
        this.thread = thread;
        this.resume = resume;
    }

    /** The token of the activity on top, or null. */
    IBinder top() {
        return entries.isEmpty() ? null : entries.get(entries.size() - 1).token;
    }

    ActivityInfo info(ComponentName component) {
        for (Entry entry : entries) {
            if (entry.info.packageName.equals(component.getPackageName()) && entry.info.name.equals(component.getClassName())) {
                return entry.info;
            }
        }
        return null;
    }

    /** The intent's activity: the one it names, else the one whose filter takes it, else the launcher's. */
    ActivityInfo resolve(Intent intent, String named) throws Refusal {
        String packageName = app.application.packageName;
        if (named != null) {
            String name = Launcher.className(packageName, named);
            if (name.equals(app.activity.name)) return app.activity;
            for (PackageParser.Activity declared : app.parsed().activities) {
                if (declared.className.equals(name)) return app.activityInfo(declared);
            }
            throw new Refusal("no_such_activity", packageName + " declares no activity " + name);
        }
        if (isLauncher(intent) || Intent.ACTION_MAIN.equals(intent.getAction()) && intent.getData() == null) {
            return app.activity;
        }
        // As startActivity resolves an implicit intent: its filters, with the default category.
        java.util.Set<String> categories = new java.util.HashSet<>();
        if (intent.getCategories() != null) categories.addAll(intent.getCategories());
        categories.add(Intent.CATEGORY_DEFAULT);
        for (PackageParser.Activity declared : app.parsed().activities) {
            for (PackageParser.ActivityIntentInfo filter : declared.intents) {
                if (filter.match(intent.getAction(), intent.getType(), intent.getScheme(), intent.getData(), categories,
                        "uxdroid") >= 0) {
                    return app.activityInfo(declared);
                }
            }
        }
        throw new Refusal("no_such_activity", "no activity of " + packageName + " takes " + intent.toUri(0));
    }

    static boolean isLauncher(Intent intent) {
        return Intent.ACTION_MAIN.equals(intent.getAction()) && intent.hasCategory(Intent.CATEGORY_LAUNCHER)
                && intent.getData() == null;
    }

    /**
     * Starts {@code intent} on {@code info}: brings the task forward for a launcher intent, hands it to
     * an existing activity's onNewIntent when its launch mode or the intent's flags say so, else launches
     * a new activity on top. Returns what it did: launched, new_intent or brought_forward.
     */
    String start(Intent intent, ActivityInfo info) throws RemoteException {
        return start(intent, info, null);
    }

    /** {@link #start}, for an activity that waits for a result from the one launched, if any. */
    private String start(Intent intent, ActivityInfo info, Result result) throws RemoteException {
        intent.setComponent(new ComponentName(info.packageName, info.name));
        if (!entries.isEmpty() && isLauncher(intent) && info.name.equals(app.activity.name)) {
            return "brought_forward";
        }
        int flags = intent.getFlags();
        boolean single = info.launchMode == ActivityInfo.LAUNCH_SINGLE_TASK
                || info.launchMode == ActivityInfo.LAUNCH_SINGLE_INSTANCE
                || info.launchMode == ActivityInfo.LAUNCH_SINGLE_INSTANCE_PER_TASK;
        boolean singleTop = single || info.launchMode == ActivityInfo.LAUNCH_SINGLE_TOP
                || (flags & Intent.FLAG_ACTIVITY_SINGLE_TOP) != 0;
        Entry existing = single || (flags & Intent.FLAG_ACTIVITY_CLEAR_TOP) != 0 ? highest(info) : null;
        if (existing != null) {
            while (entries.get(entries.size() - 1) != existing) destroy(entries.get(entries.size() - 1));
            if (singleTop) {
                deliver(existing, intent);
                return "new_intent";
            }
            // A standard activity under FLAG_ACTIVITY_CLEAR_TOP is made again for the intent.
            destroy(existing);
        } else if (!entries.isEmpty() && singleTop && entries.get(entries.size() - 1).info.name.equals(info.name)) {
            deliver(entries.get(entries.size() - 1), intent);
            return "new_intent";
        }
        launch(intent, info, result);
        return "launched";
    }

    /**
     * The app's own startActivity, which Instrumentation turns into an ActivityManager result code: an
     * intent for one of the app's activities starts as {@link #start} does, with FLAG_ACTIVITY_CLEAR_TASK
     * first finishing the task; one for another app's activity, or that none of the app's takes, has no
     * activity here, since no other app is installed, and fails with ActivityNotFoundException.
     * With {@code check}, only the manifest is
     * read, so any thread may ask, and an intent the app takes answers START_SUCCESS without starting.
     */
    int startFromApp(Intent intent, boolean check) throws RemoteException {
        return startFromApp(intent, check, null);
    }

    /**
     * {@link #startFromApp}, with what startActivityForResult asked: when the activity started finishes,
     * its result is delivered to {@code result.to} before that one resumes. A start that launches nothing
     * new, as a launch mode or flag may decide, answers RESULT_CANCELED at once, as Android does.
     */
    int startFromApp(Intent intent, boolean check, Result result) throws RemoteException {
        ComponentName component = intent.getComponent();
        String packageName = app.application.packageName;
        if (component != null && !component.getPackageName().equals(packageName)) {
            return android.app.ActivityManager.START_CLASS_NOT_FOUND;
        }
        if (intent.getPackage() != null && !intent.getPackage().equals(packageName)) {
            return android.app.ActivityManager.START_INTENT_NOT_RESOLVED;
        }
        ActivityInfo info;
        try {
            info = resolve(intent, component == null ? null : component.getClassName());
        } catch (Refusal refused) {
            return component != null ? android.app.ActivityManager.START_CLASS_NOT_FOUND
                    : android.app.ActivityManager.START_INTENT_NOT_RESOLVED;
        }
        if (check) return android.app.ActivityManager.START_SUCCESS;
        int flags = intent.getFlags();
        if ((flags & Intent.FLAG_ACTIVITY_NEW_TASK) != 0 && (flags & Intent.FLAG_ACTIVITY_CLEAR_TASK) != 0) {
            while (!entries.isEmpty()) destroy(entries.get(entries.size() - 1));
        }
        String outcome = start(intent, info, result);
        if (result != null && !outcome.equals("launched")) deliverResult(result, Activity.RESULT_CANCELED, null);
        switch (outcome) {
            case "new_intent": return android.app.ActivityManager.START_DELIVERED_TO_TOP;
            case "brought_forward": return android.app.ActivityManager.START_TASK_TO_FRONT;
            default: return android.app.ActivityManager.START_SUCCESS;
        }
    }

    /**
     * {@link #startFromApp} from any thread: the task runs on the main thread, so a start from another
     * thread is checked against the manifest now, so its caller still sees a refusal, and queued there.
     */
    int startFromAnyThread(Intent intent) throws RemoteException {
        return startFromAnyThread(intent, null);
    }

    int startFromAnyThread(Intent intent, Result result) throws RemoteException {
        if (android.os.Looper.myLooper() == android.os.Looper.getMainLooper()) return startFromApp(intent, false, result);
        int refused = startFromApp(intent, true);
        if (refused != android.app.ActivityManager.START_SUCCESS) return refused;
        Intent copy = new Intent(intent);
        new android.os.Handler(android.os.Looper.getMainLooper()).post(() -> {
            try {
                startFromApp(copy, false, result);
            } catch (RemoteException | RuntimeException e) {
                System.err.println("uxdroid: cannot start " + copy + ": " + e);
            }
        });
        return android.app.ActivityManager.START_SUCCESS;
    }

    private Entry highest(ActivityInfo info) {
        for (int i = entries.size() - 1; i >= 0; i--) {
            if (entries.get(i).info.name.equals(info.name)) return entries.get(i);
        }
        return null;
    }

    private void launch(Intent intent, ActivityInfo info, Result result) throws RemoteException {
        Entry below = entries.isEmpty() ? null : entries.get(entries.size() - 1);
        if (below != null && below.resumed) {
            ClientTransaction pause = new ClientTransaction(thread);
            pause.addTransactionItem(new PauseActivityItem(below.token));
            thread.scheduleTransaction(pause);
            below.resumed = false;
        }
        Entry entry = new Entry(info);
        entry.result = result;
        Configuration config = Launcher.configuration();
        ClientTransaction launch = new ClientTransaction(thread);
        launch.addTransactionItem(new LaunchActivityItem(entry.token, intent, 0, info, config, config,
                0, info.packageName, null, // default device, referrer, no voice interactor
                android.app.ActivityManager.PROCESS_STATE_TOP,
                null, null, null, null, null, // no saved state, results, new intents or scene transition
                true, null, new Binder(), // forward, no profiler, assist token
                Launcher.clientController(), new Binder(), false, null, new Binder(), // not from a bubble, no task fragment
                new ActivityWindowInfo()));
        if (resume) {
            // As ActivityTaskManager does for a launch in front: resume it, which adds its window.
            launch.addTransactionItem(new ResumeActivityItem(entry.token, true, false));
            entry.resumed = true;
        }
        entries.add(entry);
        thread.scheduleTransaction(launch);
    }

    private void deliver(Entry entry, Intent intent) throws RemoteException {
        ClientTransaction transaction = new ClientTransaction(thread);
        // Resumed after onNewIntent, as an activity brought to the top is.
        transaction.addTransactionItem(new NewIntentItem(entry.token,
                List.of(new ReferrerIntent(intent, app.application.packageName)), resume));
        thread.scheduleTransaction(transaction);
        entry.resumed = resume;
    }

    private void destroy(Entry entry) throws RemoteException {
        entries.remove(entry);
        ClientTransaction transaction = new ClientTransaction(thread);
        transaction.addTransactionItem(new DestroyActivityItem(entry.token, true));
        thread.scheduleTransaction(transaction);
    }

    /**
     * Hands a finished activity's result to the one that asked for it, if it is still in the task, as
     * an ActivityResultItem: the framework calls its onActivityResult before it resumes.
     */
    private void deliverResult(Result result, int resultCode, Intent data) throws RemoteException {
        for (Entry waiting : entries) {
            if (waiting.token == result.to) {
                ClientTransaction transaction = new ClientTransaction(thread);
                transaction.addTransactionItem(new ActivityResultItem(waiting.token,
                        List.of(new ResultInfo(result.who, result.requestCode, resultCode, data))));
                thread.scheduleTransaction(transaction);
                return;
            }
        }
    }

    private void resumeTop() throws RemoteException {
        if (entries.isEmpty() || !resume) return;
        Entry top = entries.get(entries.size() - 1);
        if (top.resumed) return;
        ClientTransaction transaction = new ClientTransaction(thread);
        transaction.addTransactionItem(new ResumeActivityItem(top.token, true, false));
        thread.scheduleTransaction(transaction);
        top.resumed = true;
    }

    /**
     * Activity.finish: destroys it and resumes the one below. When the last one goes, the process ends,
     * as a desktop app does when its last window closes.
     */
    boolean finish(IBinder token, int resultCode, Intent resultData) throws RemoteException {
        for (Entry entry : entries) {
            if (entry.token == token) {
                destroy(entry);
                // Before the activity below resumes, as ActivityRecord.finishIfPossible sends it.
                if (entry.result != null) deliverResult(entry.result, resultCode, resultData);
                if (entries.isEmpty()) {
                    Control.exitAfterPending();
                } else {
                    resumeTop();
                }
                return true;
            }
        }
        return false;
    }

    /** Finishes every activity from the top, then the process exits. */
    JSONObject stop() throws RemoteException, JSONException {
        int count = entries.size();
        while (!entries.isEmpty()) destroy(entries.get(entries.size() - 1));
        Control.exitAfterPending();
        return new JSONObject().put("pid", android.os.Process.myPid()).put("finished", count);
    }

    JSONObject status() throws JSONException {
        JSONArray activities = new JSONArray();
        for (Entry entry : entries) {
            activities.put(new JSONObject().put("component", entry.component())
                    .put("state", entry.resumed ? "resumed" : "paused"));
        }
        return new JSONObject().put("package", app.application.packageName)
                .put("pid", android.os.Process.myPid())
                .put("data", Launcher.dataRoot())
                .put("activities", activities);
    }
}
