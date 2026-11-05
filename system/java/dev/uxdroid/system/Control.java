package dev.uxdroid.system;

import android.content.Intent;
import android.net.Uri;
import android.os.Handler;
import android.os.Looper;
import android.os.MessageQueue;
import android.os.ParcelFileDescriptor;
import java.io.FileDescriptor;
import java.util.Iterator;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * The app's side of its control socket, on the main Looper. libuxdroid_system.so reads and checks each
 * request on its own threads, then queues it here; the Looper runs it between the app's own messages,
 * so a command touches the framework only on the thread the framework expects. Commands only start the
 * app's own activities with string data, so none can make the app run code its APK does not hold.
 */
final class Control implements MessageQueue.OnFileDescriptorEventListener {
    private Control() {}

    /** Takes the instance lock on {@code lockFile}; false if another running process holds it. */
    static native boolean claim(String lockFile);

    /** Serves the package's control socket; returns the descriptor that signals queued commands. */
    private static native int serve(String packageName);

    /**
     * The next queued command as {sequence, name, arguments JSON or null}, or null when none is left:
     * a start's or intent's intent, or another command's fields.
     */
    private static native String[] take();

    /** Answers a command: its result as JSON, or the code and message of a refusal when result is null. */
    static native void answer(long sequence, String result, String code, String message);

    /** Stops serving and removes the socket. */
    private static native void close();

    /** The intent as the protocol takes it, checked, or an IllegalArgumentException. */
    static native String intent(String json);

    /** Whether systemd started this process through the app's socket unit and passed it the socket. */
    static native boolean activated();

    /** Hands a launch to the running instance and returns its answer, or throws IllegalStateException. */
    static native String forward(String packageName, String intent, int timeoutMs);

    // Kept so its descriptor stays open while the Looper watches it.
    private static ParcelFileDescriptor watched;

    /**
     * Serves the control socket once the main thread is first idle, after the launch it is scheduling:
     * served at once, it delayed the sample app's first frame by about 5 ms, though binding takes 0.3 ms.
     * One that cannot be served costs only the control, never the app.
     */
    static void start(String packageName) {
        Looper.myQueue().addIdleHandler(() -> {
            serveNow(packageName);
            return false;
        });
    }

    private static void serveNow(String packageName) {
        try {
            int event = serve(packageName);
            watched = ParcelFileDescriptor.fromFd(event);
            Looper.getMainLooper().getQueue().addOnFileDescriptorEventListener(watched.getFileDescriptor(),
                    EVENT_INPUT, new Control());
        } catch (IllegalStateException | java.io.IOException e) {
            System.err.println("uxdroid: " + packageName + " runs without a control socket: " + e.getMessage());
        }
    }

    @Override
    public int onFileDescriptorEvents(FileDescriptor fd, int events) {
        for (String[] command = take(); command != null; command = take()) {
            run(Long.parseLong(command[0]), command[1], command[2]);
        }
        return EVENT_INPUT;
    }

    private static void run(long sequence, String name, String arguments) {
        ActivityStack stack = ActivityStack.current;
        try {
            if (stack == null) throw new ActivityStack.Refusal("busy", "the app is still starting");
            JSONObject result;
            switch (name) {
                case "status":
                    result = stack.status();
                    break;
                case "start":
                case "intent":
                    JSONObject json = new JSONObject(arguments);
                    if (name.equals("intent") && !json.has("activity") && !json.has("action") && !json.has("data")) {
                        throw new ActivityStack.Refusal("bad_request", "an intent names an activity, an action or data");
                    }
                    Intent intent = intentOf(json);
                    String outcome = stack.start(intent, stack.resolve(intent, json.optString("activity", null)));
                    result = new JSONObject().put("outcome", outcome).put("pid", android.os.Process.myPid())
                            .put("activity", intent.getComponent().flattenToShortString());
                    break;
                case "stop":
                    result = stack.stop();
                    break;
                case "ui":
                    // Answered by the agent, after the nodes settle when it turned accessibility on.
                    Agent.ui(sequence, new JSONObject(arguments));
                    return;
                case "commit":
                    result = Agent.commit(new JSONObject(arguments).getString("text"));
                    break;
                case "release":
                    result = Agent.release();
                    break;
                default:
                    throw new ActivityStack.Refusal("unknown_command", "no command " + name);
            }
            answer(sequence, result.toString(), null, null);
        } catch (ActivityStack.Refusal refusal) {
            answer(sequence, null, refusal.code, refusal.getMessage());
        } catch (JSONException | android.os.RemoteException | RuntimeException e) {
            answer(sequence, null, "failed", String.valueOf(e));
        }
    }

    /**
     * The intent a checked protocol intent describes. With no action, activity or data it is the
     * launcher's intent, as a click on the app's icon sends.
     */
    static Intent intentOf(JSONObject json) throws JSONException {
        String action = json.optString("action", null);
        Intent intent = new Intent(action != null ? action : Intent.ACTION_MAIN);
        if (json.has("data")) intent.setData(Uri.parse(json.getString("data")));
        if (action == null && !json.has("activity") && !json.has("data")) intent.addCategory(Intent.CATEGORY_LAUNCHER);
        JSONObject extras = json.optJSONObject("extras");
        if (extras != null) {
            for (Iterator<String> keys = extras.keys(); keys.hasNext(); ) {
                String key = keys.next();
                intent.putExtra(key, extras.getString(key));
            }
        }
        intent.setFlags((int) json.optLong("flags", 0));
        return intent;
    }

    /**
     * Ends the process once the transactions already scheduled on the main thread have run: closes the
     * socket first, so the answers being written are finished and no new command arrives.
     */
    static void exitAfterPending() {
        new Handler(Looper.getMainLooper()).post(() -> {
            close();
            System.err.println("uxdroid: every activity finished, exiting");
            System.exit(0);
        });
    }
}
