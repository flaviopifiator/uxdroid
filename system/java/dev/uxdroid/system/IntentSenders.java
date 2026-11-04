package dev.uxdroid.system;

import android.app.ActivityManager;
import android.app.PendingIntent;
import android.content.IIntentSender;
import android.content.Intent;
import android.os.Bundle;
import android.os.IBinder;
import java.util.ArrayList;
import java.util.List;

/**
 * The app's PendingIntents, which the activity manager keeps on Android: each is an IIntentSender in
 * this process, the same one for the same request, as PendingIntentRecord's key decides it (type,
 * request code, the intent by Intent.filterEquals), so a PendingIntent made twice compares equal and
 * FLAG_NO_CREATE, FLAG_CANCEL_CURRENT, FLAG_UPDATE_CURRENT and FLAG_ONE_SHOT mean what they do there.
 * Loop Habit Tracker stops when PendingIntent.getBroadcast gives null, as it did with no activity
 * manager to make one.
 *
 * <p>Sending one that starts an activity starts it in the app's task, and one for a broadcast delivers
 * it to the app's receivers ({@link Broadcasts}). Services are not started yet, which is said once.
 */
final class IntentSenders {
    private IntentSenders() {}

    private static final List<Sender> SENDERS = new ArrayList<>();
    private static boolean warned;

    /** One PendingIntent: what it was made for, and the binder the app holds. */
    static final class Sender {
        final int type;
        final String packageName;
        final int requestCode;
        Intent intent;
        final int flags;
        boolean canceled;
        IIntentSender binder;

        Sender(int type, String packageName, int requestCode, Intent intent, int flags) {
            this.type = type;
            this.packageName = packageName;
            this.requestCode = requestCode;
            this.intent = intent;
            this.flags = flags;
        }

        boolean sameRequest(int type, String packageName, int requestCode, Intent intent) {
            return this.type == type && this.packageName.equals(packageName) && this.requestCode == requestCode
                    && this.intent.filterEquals(intent);
        }
    }

    /** The handler of the IIntentSender binder: sending it, as PendingIntent.send does. */
    public static final class Handler {
        private final Sender sender;

        Handler(Sender sender) {
            this.sender = sender;
        }

        public void send(int code, Intent fillIn, String resolvedType, IBinder allowlistToken,
                android.content.IIntentReceiver finishedReceiver, String requiredPermission, Bundle options) {
            IntentSenders.send(sender, fillIn, finishedReceiver);
        }
    }

    /** IActivityManager.getIntentSenderWithFeature. */
    static synchronized IIntentSender get(int type, String packageName, int requestCode, Intent[] intents, int flags) {
        if (intents == null || intents.length == 0) return null;
        Intent intent = new Intent(intents[intents.length - 1]);
        Sender found = null;
        for (Sender sender : SENDERS) {
            if (!sender.canceled && sender.sameRequest(type, packageName, requestCode, intent)) found = sender;
        }
        if (found != null && (flags & PendingIntent.FLAG_CANCEL_CURRENT) != 0) {
            found.canceled = true;
            SENDERS.remove(found);
            found = null;
        }
        if ((flags & PendingIntent.FLAG_NO_CREATE) != 0) return found == null ? null : found.binder;
        if (found != null) {
            if ((flags & PendingIntent.FLAG_UPDATE_CURRENT) != 0) found.intent.replaceExtras(intent.getExtras());
            return found.binder;
        }
        Sender sender = new Sender(type, packageName, requestCode, intent, flags);
        sender.binder = Services.local(IIntentSender.class, new Handler(sender));
        SENDERS.add(sender);
        return sender.binder;
    }

    private static synchronized Sender find(IIntentSender binder) {
        if (binder == null) return null;
        for (Sender sender : SENDERS) {
            if (sender.binder.asBinder() == binder.asBinder()) return sender;
        }
        return null;
    }

    static synchronized void cancel(IIntentSender binder) {
        Sender sender = find(binder);
        if (sender != null) {
            sender.canceled = true;
            SENDERS.remove(sender);
        }
    }

    /** IActivityManager.sendIntentSender; Android's result code, 0 when sent. */
    static int send(IIntentSender binder, Intent fillIn) {
        Sender sender = find(binder);
        if (sender == null) return ActivityManager.START_CANCELED;
        return send(sender, fillIn, null);
    }

    /** Sends {@code binder}; a broadcast's {@code finishedReceiver} gets its result, as PendingIntent.send's. */
    static int send(IIntentSender binder, Intent fillIn, android.content.IIntentReceiver finishedReceiver) {
        Sender sender = find(binder);
        if (sender == null) return ActivityManager.START_CANCELED;
        return send(sender, fillIn, finishedReceiver);
    }

    private static int send(Sender sender, Intent fillIn, android.content.IIntentReceiver finishedReceiver) {
        Intent intent = new Intent(sender.intent);
        if (fillIn != null && (sender.flags & PendingIntent.FLAG_IMMUTABLE) == 0) intent.fillIn(fillIn, 0);
        if ((sender.flags & PendingIntent.FLAG_ONE_SHOT) != 0) cancel(sender.binder);
        if (sender.type == ActivityManager.INTENT_SENDER_BROADCAST) {
            try {
                // A finished receiver waits for the result, so the broadcast is ordered, as on Android.
                return Broadcasts.send(intent, finishedReceiver, 0, null, null, finishedReceiver != null, false);
            } catch (android.os.RemoteException e) {
                throw new IllegalStateException("cannot broadcast " + intent, e);
            }
        }
        if (sender.type != ActivityManager.INTENT_SENDER_ACTIVITY) {
            synchronized (IntentSenders.class) {
                if (!warned) {
                    warned = true;
                    System.err.println("uxdroid: a PendingIntent of " + sender.packageName + " for a service was sent"
                            + " but not delivered: services are not started yet");
                }
            }
            return ActivityManager.START_SUCCESS;
        }
        ActivityStack stack = ActivityStack.current;
        if (stack == null) return ActivityManager.START_CANCELED;
        try {
            return stack.startFromAnyThread(intent);
        } catch (android.os.RemoteException e) {
            throw new IllegalStateException("cannot start " + intent, e);
        }
    }

    /** IActivityManager.getInfoForIntentSender: made by the app itself, under its uid. */
    static ActivityManager.PendingIntentInfo info(IIntentSender binder, int uid) {
        Sender sender = find(binder);
        if (sender == null) return null;
        return new ActivityManager.PendingIntentInfo(sender.packageName, uid,
                (sender.flags & PendingIntent.FLAG_IMMUTABLE) != 0, sender.type);
    }

    static Intent intent(IIntentSender binder) {
        Sender sender = find(binder);
        return sender == null ? null : new Intent(sender.intent);
    }
}
