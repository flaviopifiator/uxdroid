package com.example.compat;

import android.app.Application;
import android.os.Binder;
import android.os.Process;

/** Prints what the corpus apps asked of the framework while their Application was created. */
public class CompatApplication extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        // WebView and TrafficStats refuse the system uid; an app has one of its own.
        System.out.println("identity myUid " + Process.myUid() + " callingUid " + Binder.getCallingUid()
                + " applicationInfo.uid " + getApplicationInfo().uid + " userId " + Process.myUserHandle()
                + " isApplicationUid " + Process.isApplicationUid(Process.myUid()));
    }
}
