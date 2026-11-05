package android.view;

import android.content.Context;
import android.graphics.Canvas;

/** A drawable rectangle of the screen. */
public class View {
    private final Context context;
    private int width;
    private int height;
    private int backgroundColor;
    private boolean hasBackground;

    public View(Context context) {
        this.context = context;
    }

    public Context getContext() {
        return context;
    }

    public final int getWidth() {
        return width;
    }

    public final int getHeight() {
        return height;
    }

    public void setBackgroundColor(int color) {
        backgroundColor = color;
        hasBackground = true;
    }

    protected void onDraw(Canvas canvas) {}

    public void invalidate() {}

    public void draw(Canvas canvas) {
        width = canvas.getWidth();
        height = canvas.getHeight();
        if (hasBackground) {
            canvas.drawColor(backgroundColor);
        }
        onDraw(canvas);
    }
}
