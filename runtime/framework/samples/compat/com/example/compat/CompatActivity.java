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
        // An app starts its own activities, by class and by action, as Markor's and AnkiDroid's
        // first activities do; an intent no app takes fails as on Android.
        startActivity(new android.content.Intent(this, SecondActivity.class).putExtra("by", "class"));
        startActivity(new android.content.Intent("com.example.compat.SECOND").putExtra("by", "action"));
        try {
            startActivity(new android.content.Intent(android.content.Intent.ACTION_VIEW,
                    android.net.Uri.parse("https://example.org/")));
            System.out.println("started an intent no activity takes");
        } catch (android.content.ActivityNotFoundException e) {
            System.out.println("no activity takes " + e.getMessage().replaceAll(" \\{.*", ""));
        }
    }
}
