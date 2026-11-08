package dev.uxdroid.system;

import android.graphics.Rect;
import android.os.IBinder;
import android.view.IWindow;
import android.view.IWindowFocusObserver;
import android.view.IWindowId;
import android.view.IWindowSession;
import android.view.IWindowSessionCallback;
import android.view.InputChannel;
import android.view.InsetsSourceControl;
import android.view.InsetsState;
import android.util.MergedConfiguration;
import android.view.Gravity;
import android.view.SurfaceControl;
import android.view.View;
import android.view.WindowManager;
import android.view.WindowManagerGlobal;
import android.view.WindowRelayoutResult;
import android.window.ClientWindowFrames;
import java.util.HashMap;
import java.util.Map;

/**
 * The handler of IWindowManager, and of the IWindowSession it opens: what system_server's window
 * manager answers a ViewRootImpl, in this process. Each window gets a layer at the frame its layout
 * parameters give it on the display, whose SurfaceControl natives in libuxdroid_system.so show the
 * first window as the display's Wayland toplevel and later ones, such as dialogs and popups, as
 * subsurfaces of it. A new size of the toplevel lays every window out again, and closing it closes
 * the task.
 */
public final class WindowService {
    private static final Session SESSION = new Session();
    private final IWindowSession session = Services.local(IWindowSession.class, SESSION);

    public IWindowSession openSession(IWindowSessionCallback callback) {
        return session;
    }

    /**
     * Animations run at their own speed. A default of 0 would read as animations turned off, which
     * makes every ValueAnimator jump to its end after one frame.
     */
    public float getCurrentAnimatorScale() {
        return 1f;
    }

    public float getAnimationScale(int which) {
        return 1f;
    }

    public float[] getAnimationScales() {
        return new float[] {1f, 1f, 1f};
    }

    /** The display's bounds: windows fill it, with no system bars and no cutout. */
    static Rect displayBounds() {
        return new Rect(0, 0, DisplayService.widthPixels(), DisplayService.heightPixels());
    }

    /** The compositor gave the display's toplevel a new size: every window is laid out again. */
    static void displayResized(int width, int height) {
        DisplayService.resize(width, height);
        System.out.println("uxdroid: display resized to " + width + "x" + height);
        SESSION.resizeAll();
    }

    /** The compositor closed the display's toplevel. */
    static void displayClosed() {
        Launcher.ActivityManager.closeTask();
    }

    /** The handler of IWindowSession. */
    public static final class Session {
        private final Map<IBinder, SurfaceControl> layers = new HashMap<>();
        /** The server end of each window's input channel, which its layer's Wayland window feeds. */
        private final Map<IBinder, InputChannel> inputs = new HashMap<>();
        /** Each window and its last layout parameters and requested size, to lay it out again. */
        private final Map<IBinder, Placed> windows = new HashMap<>();
        /** Each window's id, which View.getWindowId wraps and AndroidX transitions compare. */
        private final Map<IBinder, IWindowId> ids = new HashMap<>();

        private static final class Placed {
            final IWindow window;
            final WindowManager.LayoutParams attrs;
            final int requestedWidth;
            final int requestedHeight;

            Placed(IWindow window, WindowManager.LayoutParams attrs, int requestedWidth, int requestedHeight) {
                this.window = window;
                this.attrs = attrs;
                this.requestedWidth = requestedWidth;
                this.requestedHeight = requestedHeight;
            }
        }

        /**
         * The window's frame, as Android's DisplayPolicy places it: a dimension that matches its
         * parent fills the display, another takes what the window asked for, within the display, and
         * gravity with the window's x and y place it.
         */
        static Rect frame(WindowManager.LayoutParams attrs, int requestedWidth, int requestedHeight) {
            Rect display = displayBounds();
            int width = attrs.width == WindowManager.LayoutParams.MATCH_PARENT || requestedWidth <= 0
                    ? display.width() : Math.min(requestedWidth, display.width());
            int height = attrs.height == WindowManager.LayoutParams.MATCH_PARENT || requestedHeight <= 0
                    ? display.height() : Math.min(requestedHeight, display.height());
            Rect frame = new Rect();
            Gravity.apply(attrs.gravity, width, height, display, attrs.x, attrs.y, frame);
            return frame;
        }

        private static void fill(ClientWindowFrames frames, Rect frame) {
            Rect display = displayBounds();
            frames.frame.set(frame);
            frames.displayFrame.set(display);
            frames.parentFrame.set(display);
            frames.compatScale = 1f;
        }

        synchronized void resizeAll() {
            android.content.res.Configuration config = Launcher.configuration();
            for (Map.Entry<IBinder, Placed> entry : new HashMap<>(windows).entrySet()) {
                Placed placed = entry.getValue();
                ClientWindowFrames frames = new ClientWindowFrames();
                fill(frames, frame(placed.attrs, placed.requestedWidth, placed.requestedHeight));
                InsetsState insets = new InsetsState();
                insets.setDisplayFrame(displayBounds());
                try {
                    placed.window.resized(frames, false, new MergedConfiguration(config, config), insets,
                            true, false, 0, -1, false, null);
                } catch (android.os.RemoteException e) {
                    throw e.rethrowAsRuntimeException();
                }
            }
        }

        public int addToDisplayAsUser(IWindow window, WindowManager.LayoutParams attrs, int viewVisibility,
                int displayId, int userId, int requestedVisibleTypes, InputChannel outInputChannel,
                InsetsState outInsetsState, InsetsSourceControl.Array outActiveControls, Rect outAttachedFrame,
                float[] outSizeCompatScale) {
            outInsetsState.setDisplayFrame(displayBounds());
            outAttachedFrame.setEmpty(); // not attached to a parent window
            // As InputDispatcher does, the window manager keeps the server end and hands the window
            // the client end, which its ViewRootImpl reads with a WindowInputEventReceiver.
            if (outInputChannel != null) {
                String name = attrs.getTitle() != null ? attrs.getTitle().toString() : "uxdroid";
                InputChannel[] pair = InputChannel.openInputChannelPair(name);
                pair[1].copyTo(outInputChannel);
                pair[1].dispose();
                InputChannel old = inputs.put(window.asBinder(), pair[0]);
                if (old != null) old.dispose();
            }
            outSizeCompatScale[0] = 1f;
            return WindowManagerGlobal.ADD_OKAY | WindowManagerGlobal.ADD_FLAG_IN_TOUCH_MODE
                    | WindowManagerGlobal.ADD_FLAG_APP_VISIBLE;
        }

        public int addToDisplay(IWindow window, WindowManager.LayoutParams attrs, int viewVisibility, int displayId,
                int requestedVisibleTypes, InputChannel outInputChannel, InsetsState outInsetsState,
                InsetsSourceControl.Array outActiveControls, Rect outAttachedFrame, float[] outSizeCompatScale) {
            return addToDisplayAsUser(window, attrs, viewVisibility, displayId, 0, requestedVisibleTypes,
                    outInputChannel, outInsetsState, outActiveControls, outAttachedFrame, outSizeCompatScale);
        }

        public synchronized int relayout(IWindow window, WindowManager.LayoutParams attrs, int requestedWidth,
                int requestedHeight, int viewVisibility, int flags, int seq, int lastSyncSeqId,
                WindowRelayoutResult out) {
            Rect display = displayBounds();
            // ViewRootImpl passes its layout parameters only when they changed.
            Placed known = windows.get(window.asBinder());
            WindowManager.LayoutParams layout = new WindowManager.LayoutParams();
            if (known != null) layout.copyFrom(known.attrs);
            if (attrs != null) layout.copyFrom(attrs);
            attrs = layout;
            // ViewRootImpl lays its window out itself and asks for that size; the display bounds it.
            Rect frame = frame(attrs, requestedWidth, requestedHeight);
            int width = frame.width();
            int height = frame.height();
            fill(out.frames, frame);
            windows.put(window.asBinder(), new Placed(window, attrs, requestedWidth, requestedHeight));
            android.content.res.Configuration config = Launcher.configuration();
            out.mergedConfiguration.setConfiguration(config, config);
            out.insetsState.setDisplayFrame(display);
            if (viewVisibility != View.VISIBLE) {
                out.surfaceControl.release();
                return 0;
            }
            IBinder key = window.asBinder();
            SurfaceControl layer = layers.get(key);
            int result = 0;
            if (layer == null) {
                String title = attrs.getTitle() != null ? attrs.getTitle().toString() : "uxdroid";
                // A Builder makes hidden layers; the window manager shows a window's own layer.
                layer = new SurfaceControl.Builder().setName(title).setBufferSize(width, height)
                        .setFormat(attrs.format).setHidden(false).setCallsite("WindowService.relayout").build();
                layers.put(key, layer);
                WindowInput.attach(layer, inputs.get(key));
                result = WindowManagerGlobal.RELAYOUT_RES_FIRST_TIME | WindowManagerGlobal.RELAYOUT_RES_SURFACE_CHANGED;
            }
            WindowInput.place(layer, frame.left, frame.top, width, height, attrs.flags, attrs.surfaceInsets.left,
                    attrs.surfaceInsets.top);
            out.surfaceControl.copyFrom(layer, "WindowService.relayout");
            return result;
        }

        public void relayoutAsync(IWindow window, WindowManager.LayoutParams attrs, int requestedWidth,
                int requestedHeight, int viewVisibility, int flags, int seq, int lastSyncSeqId) {
            relayout(window, attrs, requestedWidth, requestedHeight, viewVisibility, flags, seq, lastSyncSeqId,
                    new WindowRelayoutResult());
        }

        /** The frame is already on screen: posting the Surface presented it. */
        public void finishDrawing(IWindow window, SurfaceControl.Transaction postDrawTransaction, int seqId) {}

        /**
         * The window's id: one token per window for as long as it is added. The display's one
         * toplevel has the focus, so every window reports it, and focus never moves to notify.
         */
        public synchronized IWindowId getWindowId(IBinder window) {
            return ids.computeIfAbsent(window, key -> new IWindowId.Stub() {
                @Override
                public void registerFocusObserver(IWindowFocusObserver observer) {}

                @Override
                public void unregisterFocusObserver(IWindowFocusObserver observer) {}

                @Override
                public boolean isFocused() {
                    return true;
                }
            });
        }

        public synchronized void remove(IBinder window) {
            windows.remove(window);
            ids.remove(window);
            SurfaceControl layer = layers.remove(window);
            if (layer != null) {
                WindowInput.attach(layer, null);
                WindowInput.close(layer);
                layer.release();
            }
            InputChannel input = inputs.remove(window);
            if (input != null) input.dispose();
        }
    }
}
