package com.example.compose;

import android.app.Activity;
import android.app.AlertDialog;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.PixelFormat;
import android.graphics.Rect;
import android.os.Bundle;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.Surface;
import android.view.SurfaceControl;
import android.view.View;
import android.view.ViewTreeObserver;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.PopupWindow;
import android.widget.TextView;

/**
 * A plain SDK Activity that composes: a layer of its own over its window, moved, scaled, cropped,
 * faded and rounded by one transaction; a dialog; a popup. It knows nothing about uxdroid. Each
 * press of N goes one step further, and it prints what it did, so a scripted compositor can check
 * the screen between steps.
 */
public class ComposeActivity extends Activity {
    static final int AMBER = 0xFFFFE082;
    static final int BLUE = 0xFF1E88E5;
    static final int GREEN = 0xFF43A047;
    private FrameLayout page;
    private TextView label;
    private SurfaceControl overlay;
    private Surface surface;
    private SurfaceControl.Transaction moved;
    private int step;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        page = new FrameLayout(this);
        page.setBackgroundColor(AMBER);
        label = new TextView(this);
        label.setText("step 0");
        label.setTextColor(Color.BLACK);
        page.addView(label, new FrameLayout.LayoutParams(300, 60, Gravity.BOTTOM | Gravity.RIGHT));
        setContentView(page);
        View decor = getWindow().getDecorView();
        decor.getViewTreeObserver().addOnGlobalLayoutListener(new ViewTreeObserver.OnGlobalLayoutListener() {
            private int width, height;

            @Override
            public void onGlobalLayout() {
                if (decor.getWidth() != width || decor.getHeight() != height) {
                    width = decor.getWidth();
                    height = decor.getHeight();
                    System.out.println("compose: window " + width + "x" + height);
                }
                if (overlay == null && getWindow().getRootSurfaceControl() != null) place();
            }
        });
    }

    /** A layer of the app's own, under its window's layer, drawn blue through its own Surface. */
    private void place() {
        overlay = new SurfaceControl.Builder().setName("overlay").setBufferSize(100, 100)
                .setFormat(PixelFormat.RGBA_8888).build();
        SurfaceControl.Transaction t = getWindow().getRootSurfaceControl().buildReparentTransaction(overlay);
        t.setVisibility(overlay, true).setLayer(overlay, 1).setPosition(overlay, 20, 20).apply();
        surface = new Surface(overlay);
        paint(BLUE);
        System.out.println("compose: placed");
    }

    private void paint(int color) {
        Canvas canvas = surface.lockCanvas(null);
        canvas.drawColor(color);
        surface.unlockCanvasAndPost(canvas);
    }

    /** Runs after the window's next frame is posted. */
    private void afterFrame(Runnable then) {
        page.getViewTreeObserver().addOnDrawListener(new ViewTreeObserver.OnDrawListener() {
            @Override
            public void onDraw() {
                ViewTreeObserver.OnDrawListener self = this;
                page.post(() -> {
                    page.getViewTreeObserver().removeOnDrawListener(self);
                    then.run();
                });
            }
        });
        label.setText("step " + step);
    }

    private void next() {
        step++;
        switch (step) {
            case 1:
                // Recorded, not applied: the window draws again and the layer must not move.
                moved = new SurfaceControl.Transaction().setPosition(overlay, 200, 300).setScale(overlay, 2, 1)
                        .setCrop(overlay, new Rect(0, 0, 60, 100)).setAlpha(overlay, 0.5f)
                        .setCornerRadius(overlay, 20);
                afterFrame(() -> System.out.println("compose: unapplied"));
                break;
            case 2:
                moved.apply();
                System.out.println("compose: applied");
                break;
            case 3:
                paint(GREEN);
                System.out.println("compose: redrawn");
                break;
            case 4: {
                AlertDialog dialog = new AlertDialog.Builder(this).setTitle("A dialog").setMessage("Tap outside")
                        .setPositiveButton("OK", null).create();
                dialog.setCanceledOnTouchOutside(true);
                dialog.setOnShowListener(d -> System.out.println("compose: dialog shown"));
                dialog.setOnCancelListener(d -> System.out.println("compose: dialog canceled"));
                dialog.show();
                break;
            }
            case 5: {
                Button button = new Button(this);
                button.setText("Popup");
                button.setBackgroundColor(GREEN);
                button.setOnClickListener(v -> System.out.println("compose: popup clicked"));
                PopupWindow popup = new PopupWindow(button, 160, 80);
                popup.showAtLocation(page, Gravity.TOP | Gravity.LEFT, 50, 500);
                button.post(() -> {
                    int[] at = new int[2];
                    button.getLocationOnScreen(at);
                    System.out.println("compose: popup at " + (at[0] + 80) + " " + (at[1] + 40));
                });
                break;
            }
            default:
                break;
        }
    }

    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        if (event.getKeyCode() == KeyEvent.KEYCODE_N && event.getAction() == KeyEvent.ACTION_UP) {
            next();
            return true;
        }
        return super.dispatchKeyEvent(event);
    }

    @Override
    protected void onDestroy() {
        System.out.println("compose: destroyed");
        super.onDestroy();
    }
}
