package com.example.compat;

import android.app.Activity;
import android.os.Bundle;
import android.widget.TextView;

/** The launcher activity: a line of text, so the app has a window like the corpus apps. */
public class CompatActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        TextView text = new TextView(this);
        text.setText("compat");
        setContentView(text);
        System.out.println("activity created " + getClass().getName());
    }
}
