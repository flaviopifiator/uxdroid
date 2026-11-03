package com.example.framework;

import android.app.Activity;
import android.os.Bundle;
import android.widget.TextView;

/** A plain SDK Activity: knows nothing about uxdroid. */
public class MainActivity extends Activity {
    /** Amber, which nothing else on the screen uses, so a screenshot can find the content view. */
    static final int BACKGROUND = 0xFFFFE082;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        TextView text = new TextView(this);
        text.setText(R.string.greeting);
        text.setTextSize(20);
        text.setBackgroundColor(BACKGROUND);
        // From targetSdk 35 windows are edge to edge: the action bar covers the content's top inset.
        text.setFitsSystemWindows(true);
        setContentView(text);
        System.out.println("activity created " + getClass().getName() + " in " + getApplication().getClass().getName()
                + " text \"" + text.getText() + "\"");
    }

    @Override
    protected void onResume() {
        super.onResume();
        System.out.println("activity resumed " + getClass().getName());
    }
}
