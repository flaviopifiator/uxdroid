package dev.uxdroid.system;

import android.os.MessageQueue;
import java.io.FileDescriptor;

/**
 * Watches the client end of an input channel on its receiver's Looper, as Android's native
 * InputEventReceiver does. libuxdroid_system.so makes one per receiver, and the native side hands
 * each event to the receiver's dispatchInputEvent.
 */
final class InputListener implements MessageQueue.OnFileDescriptorEventListener {
    private final long receiver;

    InputListener(long receiver) {
        this.receiver = receiver;
    }

    @Override
    public int onFileDescriptorEvents(FileDescriptor fd, int events) {
        return dispatch(receiver);
    }

    /** Returns the events to keep watching, or 0 once the receiver is gone. */
    private static native int dispatch(long receiver);
}
