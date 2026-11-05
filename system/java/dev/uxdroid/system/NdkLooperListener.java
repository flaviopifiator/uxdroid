package dev.uxdroid.system;

import android.os.MessageQueue;
import java.io.FileDescriptor;

/**
 * Joins an NDK ALooper to a thread's Java Looper. The native bridge's libandroid gives each such
 * looper an epoll descriptor of its own and watches it here, through the framework's
 * MessageQueue.addOnFileDescriptorEventListener, so Looper.loop wakes when an app's descriptor is
 * ready and the app's callbacks run on the Looper's thread, as on Android.
 */
final class NdkLooperListener implements MessageQueue.OnFileDescriptorEventListener {
    private final long looper;

    NdkLooperListener(long looper) {
        this.looper = looper;
    }

    @Override
    public int onFileDescriptorEvents(FileDescriptor fd, int events) {
        return dispatch(looper);
    }

    /** Runs the ready callbacks and returns the events to keep watching. Registered by the bridge. */
    private static native int dispatch(long looper);
}
