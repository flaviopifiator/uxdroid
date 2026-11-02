package android.graphics;

import dev.uxdroid.runtime.Window;

/** Draws into the window's pixels. */
public class Canvas {
    private final Window window;

    public Canvas(Window window) {
        this.window = window;
    }

    public int getWidth() {
        return window.width();
    }

    public int getHeight() {
        return window.height();
    }

    public void drawColor(int color) {
        drawRect(0, 0, getWidth(), getHeight(), color);
    }

    public void drawRect(float left, float top, float right, float bottom, Paint paint) {
        drawRect(left, top, right, bottom, paint.getColor());
    }

    public void drawText(String text, float x, float y, Paint paint) {
        Window.nDrawText(window.handle(), text, x, y, paint.getColor(), paint.getTextSize());
    }

    private void drawRect(float left, float top, float right, float bottom, int color) {
        Window.nDrawRect(window.handle(), left, top, right, bottom, color);
    }
}
