package com.example.first;

import android.app.Activity;
import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.os.Bundle;
import android.view.View;

/** Paints with a color that comes from a bionic library, proving both halves of the APK run. */
public class MainActivity extends Activity {
    static {
        System.loadLibrary("firstnative");
    }

    static native int fillColor();

    static native String label();

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        View view = new ColorView(this);
        view.setBackgroundColor(fillColor());
        setContentView(view);
        System.out.println("activity created in " + getPackageName());
    }

    private static final class ColorView extends View {
        private final Paint paint = new Paint();

        ColorView(Context context) {
            super(context);
            paint.setColor(0xFFFFFFFF);
            paint.setTextSize(24);
        }

        @Override
        protected void onDraw(Canvas canvas) {
            canvas.drawText(label(), 16, 40, paint);
        }
    }
}
