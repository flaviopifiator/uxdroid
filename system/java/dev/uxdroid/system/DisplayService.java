package dev.uxdroid.system;

import android.util.DisplayMetrics;
import android.view.Display;
import android.view.DisplayInfo;

/**
 * The handler of IDisplayManager: one display, the app's Wayland toplevel. It starts as the default
 * 160 dpi screen of 400 by 700 dp that AppContext.systemResources also describes, and takes the size
 * the compositor gives the toplevel.
 */
public final class DisplayService {
    static final int WIDTH_DP = 400;
    static final int HEIGHT_DP = 700;
    static final int DENSITY = DisplayMetrics.DENSITY_DEFAULT;
    private static final float REFRESH_RATE = 60f;

    private static volatile int width = WIDTH_DP * DENSITY / DisplayMetrics.DENSITY_DEFAULT;
    private static volatile int height = HEIGHT_DP * DENSITY / DisplayMetrics.DENSITY_DEFAULT;

    static int widthPixels() { return width; }
    static int heightPixels() { return height; }

    /** The toplevel's new size, in pixels. */
    static void resize(int widthPixels, int heightPixels) {
        width = widthPixels;
        height = heightPixels;
    }

    public int[] getDisplayIds(boolean includeDisabled) {
        return new int[] {Display.DEFAULT_DISPLAY};
    }

    public DisplayInfo getDisplayInfo(int displayId) {
        if (displayId != Display.DEFAULT_DISPLAY) return null;
        DisplayInfo info = new DisplayInfo();
        info.displayId = displayId;
        info.type = Display.TYPE_INTERNAL;
        info.name = "uxdroid";
        info.uniqueId = "local:0";
        info.state = Display.STATE_ON;
        info.committedState = Display.STATE_ON;
        int width = widthPixels();
        int height = heightPixels();
        info.logicalWidth = info.appWidth = info.smallestNominalAppWidth = info.largestNominalAppWidth = width;
        info.logicalHeight = info.appHeight = info.smallestNominalAppHeight = info.largestNominalAppHeight = height;
        info.logicalDensityDpi = DENSITY;
        info.physicalXDpi = DENSITY;
        info.physicalYDpi = DENSITY;
        Display.Mode mode = new Display.Mode(1, width, height, REFRESH_RATE);
        info.supportedModes = new Display.Mode[] {mode};
        info.appsSupportedModes = info.supportedModes;
        info.modeId = info.defaultModeId = mode.getModeId();
        info.renderFrameRate = REFRESH_RATE;
        info.supportedColorModes = new int[] {Display.COLOR_MODE_DEFAULT};
        info.brightnessMinimum = 0f;
        info.brightnessMaximum = 1f;
        info.brightnessDefault = 0.5f;
        return info;
    }
}
