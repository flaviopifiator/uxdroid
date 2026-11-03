package com.example.input;

import android.app.Activity;
import android.graphics.Color;
import android.os.Bundle;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.window.OnBackInvokedDispatcher;

/**
 * A plain SDK Activity with a button and a text field: knows nothing about uxdroid. It prints what
 * it receives, and the button turns the page green, so a screenshot shows the click.
 */
public class InputActivity extends Activity {
    static final int BEFORE = 0xFFFFE082; // amber
    static final int AFTER = 0xFF66BB6A; // green
    private LinearLayout page;
    private int clicks;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        page = new LinearLayout(this);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setBackgroundColor(BEFORE);
        page.setFitsSystemWindows(true);
        Button button = new Button(this);
        button.setText("Tap");
        button.setOnClickListener(v -> {
            clicks++;
            page.setBackgroundColor(clicks % 2 == 1 ? AFTER : BEFORE);
            System.out.println("input: clicked " + clicks);
        });
        EditText field = new EditText(this);
        field.setHint("Type here");
        field.setTextColor(Color.BLACK);
        field.addTextChangedListener(new TextWatcher() {
            @Override public void beforeTextChanged(CharSequence s, int start, int count, int after) {}
            @Override public void onTextChanged(CharSequence s, int start, int before, int count) {}
            @Override public void afterTextChanged(Editable s) {
                System.out.println("input: text \"" + s + "\"");
            }
        });
        field.setOnFocusChangeListener((v, focused) -> System.out.println("input: field focused " + focused));
        page.addView(button, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 120));
        page.addView(field, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 120));
        setContentView(page);
        page.getViewTreeObserver().addOnGlobalLayoutListener(() -> {
            report("button", button);
            report("field", field);
        });
        getOnBackInvokedDispatcher().registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                () -> System.out.println("input: back"));
    }

    private static void report(String name, View view) {
        int[] at = new int[2];
        view.getLocationInWindow(at);
        System.out.println("input: layout " + name + " " + (at[0] + view.getWidth() / 2) + " "
                + (at[1] + view.getHeight() / 2));
    }

    /** Without predictive back, the back key reaches the Activity here; staying open keeps the test going. */
    @Override
    @SuppressWarnings("deprecation")
    public void onBackPressed() {
        System.out.println("input: back");
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        System.out.println("input: window focus " + hasFocus);
    }

    @Override
    public boolean dispatchTouchEvent(MotionEvent event) {
        int action = event.getActionMasked();
        if (action == MotionEvent.ACTION_DOWN || action == MotionEvent.ACTION_UP) {
            System.out.println("input: motion " + MotionEvent.actionToString(action) + " source 0x"
                    + Integer.toHexString(event.getSource()) + " at " + (int) event.getX() + "," + (int) event.getY());
        }
        return super.dispatchTouchEvent(event);
    }

    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        if (event.getAction() == KeyEvent.ACTION_DOWN) {
            System.out.println("input: key " + KeyEvent.keyCodeToString(event.getKeyCode()) + " char "
                    + event.getUnicodeChar() + " meta 0x" + Integer.toHexString(event.getMetaState()));
        }
        return super.dispatchKeyEvent(event);
    }
}
