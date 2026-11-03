package com.example.framework;

import android.app.Activity;
import android.os.Bundle;
import android.widget.TextView;

/** A plain SDK Activity: knows nothing about uxdroid. */
public class MainActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        TextView text = new TextView(this);
        text.setText(R.string.greeting);
        text.setTextSize(20);
        setContentView(text);
        System.out.println("activity created " + getClass().getName() + " in " + getApplication().getClass().getName()
                + " text \"" + text.getText() + "\"");
    }
}
