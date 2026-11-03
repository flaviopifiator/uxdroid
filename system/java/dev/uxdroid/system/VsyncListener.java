package dev.uxdroid.system;

import android.os.MessageQueue;
import java.io.FileDescriptor;

/**
 * Watches a DisplayEventReceiver's vsync timer on its Looper. libuxdroid_system.so makes one per
 * receiver and hands it to MessageQueue.addOnFileDescriptorEventListener; the native side then calls
 * the receiver's dispatchVsync, as Android's own receiver does when SurfaceFlinger's event arrives.
 */
final class VsyncListener implements MessageQueue.OnFileDescriptorEventListener {
    private final long receiver;

    VsyncListener(long receiver) {
        this.receiver = receiver;
    }

    @Override
    public int onFileDescriptorEvents(FileDescriptor fd, int events) {
        return dispatch(receiver);
    }

    /** Returns the events to keep watching, or 0 once the receiver is gone. */
    private static native int dispatch(long receiver);

    /**
     * The address of the frame clock in libuxdroid_system.so, which the NDK's AChoreographer, in
     * another library, follows too: the compositor's frame callbacks, else a 60 Hz clock.
     */
    static native long clock();
}
