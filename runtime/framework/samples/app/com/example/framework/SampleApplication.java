package com.example.framework;

import android.app.Application;

/** Proves the framework made the app's own Application before any Activity. */
public class SampleApplication extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        System.out.println("application created " + getClass().getName() + " label " + getApplicationInfo().loadLabel(getPackageManager()));
    }
}
