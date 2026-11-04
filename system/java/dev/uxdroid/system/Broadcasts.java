package dev.uxdroid.system;

import android.app.IApplicationThread;
import android.app.ReceiverInfo;
import android.content.IIntentReceiver;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageParser;
import android.content.res.CompatibilityInfo;
import android.os.Bundle;
import android.os.IBinder;
import android.os.RemoteException;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.List;

/**
 * Broadcasts inside the app, the part of Android's activity manager that keeps receivers and delivers
 * intents to them, with no other process to reach: an app's broadcasts go to its own receivers, the
 * ones it registered and the ones its manifest declares.
 *
 * <p>As on Android: a registered receiver gets every broadcast its filter matches; a manifest receiver
 * gets only an explicit one, naming the app's package or the receiver, since Android 8.0 keeps implicit
 * broadcasts from manifest receivers. A normal broadcast reaches all its receivers at once, each on the
 * main thread through the framework's own dispatchers; an ordered one goes to one receiver at a time,
 * by priority, each passing its result to the next, then to the sender's result receiver. A sticky
 * broadcast is kept, and a receiver registered later gets the last one its filter matches. Permissions
 * are not checked, since sender and receiver are one app.
 */
final class Broadcasts {
    private Broadcasts() {}

    /** A receiver the app registered with Context.registerReceiver. */
    private static final class Registered {
        final IIntentReceiver receiver;
        final IntentFilter filter;

        Registered(IIntentReceiver receiver, IntentFilter filter) {
            this.receiver = receiver;
            this.filter = filter;
        }
    }

    /** One receiver of one broadcast: registered, or of the manifest. */
    private static final class Target {
        final IIntentReceiver receiver;
        final ActivityInfo component;
        final int priority;

        Target(IIntentReceiver receiver, ActivityInfo component, int priority) {
            this.receiver = receiver;
            this.component = component;
            this.priority = priority;
        }
    }

    /** An ordered broadcast on its way through its receivers. */
    private static final class Ordered {
        final Intent intent;
        final ArrayDeque<Target> targets;
        final IIntentReceiver resultTo;
        int code;
        String data;
        Bundle extras;

        Ordered(Intent intent, List<Target> targets, IIntentReceiver resultTo, int code, String data, Bundle extras) {
            this.intent = intent;
            this.targets = new ArrayDeque<>(targets);
            this.resultTo = resultTo;
            this.code = code;
            this.data = data;
            this.extras = extras;
        }
    }

    private static Launcher.App app;
    private static IApplicationThread thread;
    private static final List<Registered> REGISTERED = new ArrayList<>();
    private static final List<Intent> STICKY = new ArrayList<>();
    /** Ordered broadcasts go one at a time, as Android's queue for one process does. */
    private static final ArrayDeque<Ordered> ORDERED = new ArrayDeque<>();
    private static Target delivering;

    /** Once the app is bound: its manifest's receivers can be made from then on. */
    static synchronized void attach(Launcher.App bound, IApplicationThread applicationThread) {
        app = bound;
        thread = applicationThread;
    }

    /** IActivityManager.registerReceiverWithFeature: the last sticky broadcast it matches, if any. */
    static Intent register(IIntentReceiver receiver, IntentFilter filter) throws RemoteException {
        Intent sticky = null;
        synchronized (Broadcasts.class) {
            if (receiver == null) return lastSticky(filter);
            REGISTERED.add(new Registered(receiver, filter));
            sticky = lastSticky(filter);
        }
        if (sticky != null) receiver.performReceive(new Intent(sticky), 0, null, null, false, true, 0);
        return sticky;
    }

    private static Intent lastSticky(IntentFilter filter) {
        for (int i = STICKY.size() - 1; i >= 0; i--) {
            if (matches(filter, STICKY.get(i))) return STICKY.get(i);
        }
        return null;
    }

    static synchronized void unregister(IIntentReceiver receiver) {
        REGISTERED.removeIf(registered -> registered.receiver.asBinder() == receiver.asBinder());
    }

    private static boolean matches(IntentFilter filter, Intent intent) {
        return filter.match(intent.getAction(), intent.getType(),
                intent.getScheme(), intent.getData(), intent.getCategories(), "uxdroid") >= 0;
    }

    /**
     * IActivityManager.broadcastIntentWithFeature and a broadcast PendingIntent: delivers {@code intent}
     * to the app's receivers, in order when {@code ordered} or when a result receiver waits.
     */
    static int send(Intent intent, IIntentReceiver resultTo, int code, String data, Bundle extras, boolean ordered,
            boolean sticky) throws RemoteException {
        List<Target> targets;
        synchronized (Broadcasts.class) {
            if (sticky) {
                STICKY.removeIf(kept -> kept.filterEquals(intent));
                STICKY.add(new Intent(intent));
            }
            targets = targets(intent);
            if (ordered || resultTo != null) {
                targets.sort((a, b) -> Integer.compare(b.priority, a.priority));
                ORDERED.add(new Ordered(new Intent(intent), targets, resultTo, code, data, extras));
                if (delivering != null || ORDERED.size() > 1) return android.app.ActivityManager.BROADCAST_SUCCESS;
            }
        }
        if (ordered || resultTo != null) {
            next();
            return android.app.ActivityManager.BROADCAST_SUCCESS;
        }
        for (Target target : targets) deliver(target, new Intent(intent), code, data, extras, false, sticky);
        return android.app.ActivityManager.BROADCAST_SUCCESS;
    }

    /** The receivers of {@code intent} in this app, registered first, then those of the manifest. */
    private static List<Target> targets(Intent intent) {
        List<Target> targets = new ArrayList<>();
        String own = app == null ? null : app.application.packageName;
        if (intent.getComponent() == null && (intent.getPackage() == null || intent.getPackage().equals(own))) {
            for (Registered registered : REGISTERED) {
                if (matches(registered.filter, intent)) {
                    targets.add(new Target(registered.receiver, null, registered.filter.getPriority()));
                }
            }
        }
        if (app == null) return targets;
        boolean explicit = intent.getComponent() != null || own.equals(intent.getPackage())
                || (intent.getFlags() & Intent.FLAG_RECEIVER_INCLUDE_BACKGROUND) != 0;
        if (!explicit || (intent.getComponent() != null && !intent.getComponent().getPackageName().equals(own))) {
            return targets;
        }
        for (PackageParser.Activity declared : app.parsed().receivers) {
            if (intent.getComponent() != null) {
                if (declared.className.equals(intent.getComponent().getClassName())) {
                    targets.add(new Target(null, app.activityInfo(declared), 0));
                }
                continue;
            }
            for (PackageParser.ActivityIntentInfo filter : declared.intents) {
                if (matches(filter, intent)) {
                    targets.add(new Target(null, app.activityInfo(declared), filter.getPriority()));
                    break;
                }
            }
        }
        return targets;
    }

    /** Hands one broadcast to one receiver; the framework runs it on the main thread. */
    private static void deliver(Target target, Intent intent, int code, String data, Bundle extras, boolean ordered,
            boolean sticky) throws RemoteException {
        if (target.receiver != null) {
            target.receiver.performReceive(intent, code, data, extras, ordered, sticky, 0);
            return;
        }
        intent.setComponent(new android.content.ComponentName(target.component.packageName, target.component.name));
        ReceiverInfo info = new ReceiverInfo();
        info.intent = intent;
        info.activityInfo = target.component;
        info.compatInfo = CompatibilityInfo.DEFAULT_COMPATIBILITY_INFO;
        info.resultCode = code;
        info.data = data;
        info.extras = extras;
        info.sync = ordered;
        info.ordered = ordered;
        // A normal broadcast needs no answer; an ordered one waits for finishReceiver.
        info.assumeDelivered = !ordered;
        info.sendingUser = 0;
        info.processState = android.app.ActivityManager.PROCESS_STATE_TOP;
        info.sendingUid = app.application.uid;
        info.sendingPackage = app.application.packageName;
        info.registered = false;
        thread.scheduleReceiverList(List.of(info));
    }

    /** IActivityManager.finishReceiver: the receiver of the ordered broadcast in flight is done. */
    static void finished(IBinder who, int code, String data, Bundle extras, boolean abort) throws RemoteException {
        synchronized (Broadcasts.class) {
            Ordered current = ORDERED.peek();
            if (current == null || delivering == null) return;
            current.code = code;
            current.data = data;
            current.extras = extras;
            if (abort) current.targets.clear();
            delivering = null;
        }
        next();
    }

    /** Moves the ordered broadcast in front to its next receiver, or to its result, then the next one. */
    private static void next() throws RemoteException {
        while (true) {
            Ordered current;
            Target target;
            synchronized (Broadcasts.class) {
                current = ORDERED.peek();
                if (current == null || delivering != null) return;
                target = current.targets.poll();
                if (target != null) {
                    delivering = target;
                } else {
                    ORDERED.poll();
                }
            }
            if (target != null) {
                deliver(target, new Intent(current.intent), current.code, current.data, current.extras, true, false);
                return;
            }
            if (current.resultTo != null) {
                // The sender's result receiver is registered for this broadcast only: not ordered, so it
                // does not answer.
                current.resultTo.performReceive(new Intent(current.intent), current.code, current.data,
                        current.extras, false, false, 0);
            }
        }
    }
}
