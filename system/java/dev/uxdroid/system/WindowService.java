package dev.uxdroid.system;

import android.graphics.Rect;
import android.os.IBinder;
import android.view.IWindow;
import android.view.IWindowSession;
import android.view.IWindowSessionCallback;
import android.view.InputChannel;
import android.view.InsetsSourceControl;
import android.view.InsetsState;
import android.view.SurfaceControl;
import android.view.View;
import android.view.WindowManager;
import android.view.WindowManagerGlobal;
import android.view.WindowRelayoutResult;
import java.util.HashMap;
import java.util.Map;

/**
 * The handler of IWindowManager, and of the IWindowSession it opens: what system_server's window
 * manager answers a ViewRootImpl, in this process. Each window gets a layer the size of the display,
 * whose SurfaceControl natives in libuxdroid_system.so show it as a Wayland toplevel.
 */
public final class WindowService {
    private final IWindowSession session = Services.local(IWindowSession.class, new Session());

    public IWindowSession openSession(IWindowSessionCallback callback) {
        return session;
    }

    /** The display's bounds: windows fill it, with no system bars and no cutout. */
    static Rect displayBounds() {
        return new Rect(0, 0, DisplayService.widthPixels(), DisplayService.heightPixels());
    }

    /** The handler of IWindowSession. */
    public static final class Session {
        private final Map<IBinder, SurfaceControl> layers = new HashMap<>();

        public int addToDisplayAsUser(IWindow window, WindowManager.LayoutParams attrs, int viewVisibility,
                int displayId, int userId, int requestedVisibleTypes, InputChannel outInputChannel,
                InsetsState outInsetsState, InsetsSourceControl.Array outActiveControls, Rect outAttachedFrame,
                float[] outSizeCompatScale) {
            outInsetsState.setDisplayFrame(displayBounds());
            outAttachedFrame.setEmpty(); // not attached to a parent window
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

        public int relayout(IWindow window, WindowManager.LayoutParams attrs, int requestedWidth, int requestedHeight,
                int viewVisibility, int flags, int seq, int lastSyncSeqId, WindowRelayoutResult out) {
            Rect display = displayBounds();
            // ViewRootImpl lays its window out itself and asks for that size; the display bounds it.
            int width = requestedWidth > 0 ? Math.min(requestedWidth, display.width()) : display.width();
            int height = requestedHeight > 0 ? Math.min(requestedHeight, display.height()) : display.height();
            out.frames.frame.set(0, 0, width, height);
            out.frames.displayFrame.set(display);
            out.frames.parentFrame.set(display);
            out.frames.compatScale = 1f;
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
                layer = new SurfaceControl.Builder().setName(title).setBufferSize(width, height)
                        .setFormat(attrs.format).setCallsite("WindowService.relayout").build();
                layers.put(key, layer);
                result = WindowManagerGlobal.RELAYOUT_RES_FIRST_TIME | WindowManagerGlobal.RELAYOUT_RES_SURFACE_CHANGED;
            }
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

        public void remove(IBinder window) {
            SurfaceControl layer = layers.remove(window);
            if (layer != null) layer.release();
        }
    }
}
