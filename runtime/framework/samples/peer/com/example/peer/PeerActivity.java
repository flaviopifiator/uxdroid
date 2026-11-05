package com.example.peer;

import android.app.Activity;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.widget.TextView;

/** Looks the compat sample up from another app: what it set and declared, as compat_smoke.sh checks. */
public class PeerActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        TextView text = new TextView(this);
        text.setText("peer");
        setContentView(text);
        PackageManager pm = getPackageManager();
        ComponentName toggled = new ComponentName("com.example.compat", "com.example.compat.ToggledActivity");
        try {
            int found = pm.queryIntentActivities(new Intent("com.example.compat.TOGGLED"), 0).size();
            System.out.println("peer sees compat toggled " + pm.getComponentEnabledSetting(toggled) + " found " + found);
        } catch (RuntimeException e) {
            System.out.println("peer sees compat toggled failed " + e);
        }
        try {
            Intent launch = pm.getLaunchIntentForPackage("com.example.compat");
            Bundle metaData = pm.getApplicationInfo("com.example.compat", PackageManager.GET_META_DATA).metaData;
            System.out.println("peer resolves compat launcher "
                    + (launch == null ? "none" : launch.getComponent().getClassName()) + " auto-init "
                    + (metaData == null ? "missing" : String.valueOf(metaData.getBoolean("com.example.compat.auto-init", true))));
        } catch (PackageManager.NameNotFoundException | RuntimeException e) {
            System.out.println("peer resolves compat failed " + e);
        }
        Intent own = pm.getLaunchIntentForPackage(getPackageName());
        System.out.println("peer own launcher " + (own == null ? "none" : own.getComponent().getClassName()));
    }
}
