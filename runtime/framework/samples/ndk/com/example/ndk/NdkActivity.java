package com.example.ndk;

import android.app.Activity;
import android.graphics.Bitmap;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.SurfaceHolder;

/**
 * An Activity whose drawing is native, as NativeActivity and game engines do: it takes its window's
 * surface, then draws one frame with ANativeWindow and, later, one with EGL and GLES. Knows nothing
 * about uxdroid.
 */
public class NdkActivity extends Activity implements SurfaceHolder.Callback2 {
    /** Teal, 0xAARRGGBB, the frame drawn with ANativeWindow_lock. */
    static final int CPU_COLOR = 0xFF00897B;
    /** How long the CPU frame stays before the EGL frame replaces it, long enough for a screenshot. */
    static final int GL_DELAY_MILLIS = 4000;

    static {
        System.loadLibrary("ndksample");
    }

    /** Exercises liblog, the asset manager, a Bitmap, OpenSL ES and the main ALooper. */
    static native String check(android.content.res.AssetManager assets, Bitmap bitmap);

    /** Fills the surface with a color through ANativeWindow_lock and unlockAndPost. */
    static native String drawCpu(android.view.Surface surface, int color);

    /** Clears the surface through EGL and GLES, with the top-left quarter white. */
    static native String drawGl(android.view.Surface surface, float red, float green, float blue);

    private final Handler handler = new Handler(Looper.getMainLooper());
    private boolean drawn;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().takeSurface(this);
        Bitmap bitmap = Bitmap.createBitmap(2, 1, Bitmap.Config.ARGB_8888);
        bitmap.setPixel(0, 0, 0xFF102030);
        System.out.println("ndk check: " + check(getAssets(), bitmap));
        System.out.println("ndk bitmap pixel written natively: " + Integer.toHexString(bitmap.getPixel(1, 0)));
    }

    @Override
    public void surfaceCreated(SurfaceHolder holder) {}

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
        if (drawn) return;
        drawn = true;
        handler.post(() -> {
            System.out.println("ndk cpu frame: " + drawCpu(holder.getSurface(), CPU_COLOR));
            handler.postDelayed(() -> System.out.println(
                    "ndk egl frame: " + drawGl(holder.getSurface(), 0.8f, 0.2f, 0.6f)), GL_DELAY_MILLIS);
        });
    }

    @Override
    public void surfaceRedrawNeeded(SurfaceHolder holder) {}

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {}
}
