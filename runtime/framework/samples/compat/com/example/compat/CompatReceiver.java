package com.example.compat;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** A receiver of the manifest, as Loop Habit Tracker's reminders and widgets declare. */
public class CompatReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context context, Intent intent) {
        System.out.println("broadcast manifest " + intent.getAction() + " by " + intent.getStringExtra("by")
                + " main " + (android.os.Looper.myLooper() == android.os.Looper.getMainLooper()));
    }
}
