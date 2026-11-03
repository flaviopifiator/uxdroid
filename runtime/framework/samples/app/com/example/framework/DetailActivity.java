package com.example.framework;

import android.app.Activity;
import android.os.Bundle;
import android.widget.TextView;

/** A second Activity of the sample, which a running app's control socket starts. */
public class DetailActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        TextView text = new TextView(this);
        text.setText("detail");
        text.setFitsSystemWindows(true);
        setContentView(text);
        System.out.println("activity created " + getClass().getName() + " " + MainActivity.describe(getIntent()));
    }

    @Override
    protected void onResume() {
        super.onResume();
        System.out.println("activity resumed " + getClass().getName());
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        System.out.println("activity destroyed " + getClass().getName());
    }
}
