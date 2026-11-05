package dev.uxdroid.runtime;

import android.app.Activity;
import android.graphics.Canvas;
import android.view.View;

/** Starts an Activity and keeps it on screen until its window closes. */
public final class Launcher {
    private static final int DEFAULT_WIDTH = 640;
    private static final int DEFAULT_HEIGHT = 480;

    private Launcher() {}

    public static void main(String[] args) throws Exception {
        if (args.length != 1) {
            throw new IllegalArgumentException("expected the Activity class name");
        }
        Activity activity = (Activity) Class.forName(args[0]).getDeclaredConstructor().newInstance();
        activity.dispatchLifecycle(0);

        Window window = new Window(activity.getPackageName(), DEFAULT_WIDTH, DEFAULT_HEIGHT);
        Canvas canvas = new Canvas(window);
        boolean exitAfterFrame = Boolean.getBoolean("uxdroid.exit_after_first_frame");
        int state = Window.NEEDS_REDRAW;
        while (state != Window.CLOSED && !activity.isFinishing()) {
            if (state == Window.NEEDS_REDRAW) {
                View view = activity.getContentView();
                if (view != null) {
                    view.draw(canvas);
                }
                window.present();
                System.out.println("frame presented");
                if (exitAfterFrame) {
                    break;
                }
            }
            state = window.waitForEvents();
        }
        activity.dispatchLifecycle(1);
        System.exit(0);
    }
}
