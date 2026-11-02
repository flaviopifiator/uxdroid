package android.widget;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.view.View;

/** Draws one line of text. */
public class TextView extends View {
    private final Paint paint = new Paint();
    private CharSequence text = "";

    public TextView(Context context) {
        super(context);
        paint.setTextSize(16);
    }

    public void setText(CharSequence text) {
        this.text = text;
    }

    public CharSequence getText() {
        return text;
    }

    public void setTextColor(int color) {
        paint.setColor(color);
    }

    public void setTextSize(float size) {
        paint.setTextSize(size);
    }

    @Override
    protected void onDraw(Canvas canvas) {
        canvas.drawText(text.toString(), 0, paint.getTextSize(), paint);
    }
}
