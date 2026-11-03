package dev.uxdroid.system;

import android.os.MessageQueue;
import android.view.InputChannel;
import android.view.SurfaceControl;
import java.io.FileDescriptor;

/**
 * Watches a window's Wayland connection on the Looper that draws it. libuxdroid_system.so makes one
 * per window; reading the connection answers the compositor and sends the seat's events down the
 * input channel the window manager attached to the window's layer.
 */
final class WindowInput implements MessageQueue.OnFileDescriptorEventListener {
    private final long window;

    WindowInput(long window) {
        this.window = window;
    }

    @Override
    public int onFileDescriptorEvents(FileDescriptor fd, int events) {
        return dispatch(window);
    }

    /** Returns the events to keep watching, or 0 once the window is gone. */
    private static native int dispatch(long window);

    /** Sends the input of the layer's window to the server end of its channel; null detaches it. */
    static native void attach(SurfaceControl layer, InputChannel server);
}
