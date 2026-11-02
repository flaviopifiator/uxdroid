package dev.uxdroid.runtime;

/** A window on the host's display, implemented by libuxdroid_platform.so. */
public final class Window {
    public static final int RUNNING = 0;
    public static final int NEEDS_REDRAW = 1;
    public static final int CLOSED = 2;

    static {
        System.loadLibrary("uxdroid_platform");
    }

    private final long handle;

    public Window(String title, int width, int height) {
        handle = nCreate(title, width, height);
    }

    public long handle() {
        return handle;
    }

    public int width() {
        return nWidth(handle);
    }

    public int height() {
        return nHeight(handle);
    }

    public void present() {
        nPresent(handle);
    }

    /** Waits for the display and returns one of the states above. */
    public int waitForEvents() {
        return nWaitForEvents(handle);
    }

    private static native long nCreate(String title, int width, int height);

    private static native int nWidth(long handle);

    private static native int nHeight(long handle);

    private static native void nPresent(long handle);

    private static native int nWaitForEvents(long handle);

    public static native void nDrawRect(
            long handle, float left, float top, float right, float bottom, int color);

    public static native void nDrawText(
            long handle, String text, float x, float y, int color, float size);
}
