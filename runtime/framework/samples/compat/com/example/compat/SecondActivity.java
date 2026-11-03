package com.example.compat;

import android.app.Activity;
import android.os.Bundle;

/** An activity the launcher activity starts. */
public class SecondActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        System.out.println("activity created " + getClass().getName() + " by " + getIntent().getStringExtra("by"));
    }
}
