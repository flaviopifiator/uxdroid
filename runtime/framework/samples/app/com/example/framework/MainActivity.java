package com.example.framework;

import android.app.Activity;
import android.content.Intent;
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

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        System.out.println("activity new intent " + getClass().getName() + " " + describe(intent));
    }

    @Override
    protected void onPause() {
        super.onPause();
        System.out.println("activity paused " + getClass().getName());
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        System.out.println("activity destroyed " + getClass().getName());
    }

    /** The action, data and extras of an intent, extras sorted by key. */
    static String describe(Intent intent) {
        StringBuilder text = new StringBuilder("action ").append(intent.getAction()).append(" data ").append(intent.getData());
        Bundle extras = intent.getExtras();
        if (extras != null) {
            for (String key : new java.util.TreeSet<>(extras.keySet())) {
                text.append(' ').append(key).append('=').append(extras.get(key));
            }
        }
        return text.toString();
    }
}
