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
        database();
    }

    /** Loop Habit Tracker opens its database while its Application is created. */
    private void database() {
        android.database.sqlite.SQLiteOpenHelper helper =
                new android.database.sqlite.SQLiteOpenHelper(this, "compat.db", null, 1) {
            @Override
            public void onCreate(android.database.sqlite.SQLiteDatabase db) {
                db.execSQL("CREATE TABLE habit (name TEXT)");
            }

            @Override
            public void onUpgrade(android.database.sqlite.SQLiteDatabase db, int from, int to) {}
        };
        android.database.sqlite.SQLiteDatabase db = helper.getWritableDatabase();
        db.execSQL("INSERT INTO habit VALUES ('read')");
        try (android.database.Cursor rows = db.rawQuery("SELECT count(*) FROM habit", null)) {
            rows.moveToFirst();
            System.out.println("database " + db.getPath() + " rows " + rows.getInt(0) + " page size " + db.getPageSize());
        }
        helper.close();
    }
}
