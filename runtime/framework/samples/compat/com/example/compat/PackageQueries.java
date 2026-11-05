package com.example.compat;

import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.net.Uri;
import android.os.Bundle;
import java.util.List;

/**
 * What an app store asks the PackageManager about itself and the other installed apps, here the peer
 * sample installed beside this one. Each check runs on its own, so one that fails does not hide others.
 */
final class PackageQueries {
    private PackageQueries() {}

    interface Check {
        void run() throws Exception;
    }

    private static void check(String name, Check check) {
        try {
            check.run();
        } catch (Exception e) {
            System.out.println("packages " + name + " failed " + e);
        }
    }

    private static String names(List<ResolveInfo> found) {
        StringBuilder text = new StringBuilder();
        for (ResolveInfo info : found) {
            String name = info.activityInfo != null ? info.activityInfo.name
                    : info.serviceInfo != null ? info.serviceInfo.name : info.providerInfo.name;
            text.append(text.length() == 0 ? "" : ",").append(name);
        }
        return text.length() == 0 ? "none" : text.toString();
    }

    static void print(Context context) {
        PackageManager pm = context.getPackageManager();
        String own = context.getPackageName();
        // As Sentry reads io.sentry.auto-init: meta-data only with GET_META_DATA.
        check("meta-data", () -> {
            Bundle metaData = pm.getApplicationInfo(own, PackageManager.GET_META_DATA).metaData;
            System.out.println("packages meta-data auto-init "
                    + (metaData == null ? "missing" : String.valueOf(metaData.getBoolean("com.example.compat.auto-init", true)))
                    + " without flag " + pm.getApplicationInfo(own, 0).metaData);
        });
        check("installed", () -> {
            StringBuilder text = new StringBuilder();
            for (android.content.pm.PackageInfo info : pm.getInstalledPackages(0)) {
                text.append(text.length() == 0 ? "" : ",").append(info.packageName);
            }
            System.out.println("packages installed " + text + " applications " + pm.getInstalledApplications(0).size());
        });
        check("peer", () -> {
            android.content.pm.PackageInfo info = pm.getPackageInfo("com.example.peer", 0);
            Bundle metaData = pm.getApplicationInfo("com.example.peer", PackageManager.GET_META_DATA).metaData;
            System.out.println("packages peer version " + info.versionName + " " + info.getLongVersionCode()
                    + " meta-data " + (metaData == null ? "missing" : metaData.getString("com.example.peer.flavor")));
        });
        check("uid", () -> {
            int uid = pm.getPackageUid("com.example.peer", 0);
            String[] named = pm.getPackagesForUid(uid);
            System.out.println("packages uid peer application " + (uid >= 10000 && uid <= 19999 && uid != android.os.Process.myUid())
                    + " named " + (named == null ? "none" : String.join(",", named)));
        });
        check("missing", () -> {
            try {
                pm.getPackageInfo("com.example.absent", 0);
                System.out.println("packages missing found");
            } catch (PackageManager.NameNotFoundException e) {
                System.out.println("packages missing not found");
            }
        });
        check("launch", () -> {
            Intent peer = pm.getLaunchIntentForPackage("com.example.peer");
            Intent self = pm.getLaunchIntentForPackage(own);
            System.out.println("packages launch peer " + (peer == null ? "none" : peer.getComponent().getClassName())
                    + " own " + (self == null ? "none" : self.getComponent().getClassName()));
        });
        check("view", () -> {
            Intent view = new Intent(Intent.ACTION_VIEW, Uri.parse("peer://open"));
            ResolveInfo resolved = pm.resolveActivity(view, PackageManager.MATCH_DEFAULT_ONLY);
            System.out.println("packages view peer handlers " + names(pm.queryIntentActivities(view, PackageManager.MATCH_DEFAULT_ONLY))
                    + " resolved " + (resolved == null ? "none" : resolved.activityInfo.name)
                    + " https " + names(pm.queryIntentActivities(new Intent(Intent.ACTION_VIEW,
                            Uri.parse("https://example.org/")), PackageManager.MATCH_DEFAULT_ONLY)));
        });
        check("service", () -> {
            List<ResolveInfo> found = pm.queryIntentServices(new Intent("com.example.peer.SERVICE"), 0);
            android.content.pm.ServiceInfo info = pm.getServiceInfo(
                    new ComponentName("com.example.peer", "com.example.peer.PeerService"), PackageManager.GET_META_DATA);
            System.out.println("packages service " + names(found) + " meta-data "
                    + (info.metaData == null ? "missing" : info.metaData.getString("com.example.peer.service")));
        });
        check("providers", () -> System.out.println("packages providers "
                + names(pm.queryIntentContentProviders(new Intent("com.example.peer.PROVIDE"), 0))));
        check("activity", () -> {
            android.content.pm.ActivityInfo info = pm.getActivityInfo(
                    new ComponentName("com.example.peer", "com.example.peer.ViewerActivity"), 0);
            System.out.println("packages activity " + info.name + " exported " + info.exported
                    + " application " + info.applicationInfo.packageName);
        });
        // The app turns one of its components off, which lasts: the peer app reads it on its next start.
        check("toggled", () -> {
            ComponentName toggled = new ComponentName(context, ToggledActivity.class);
            Intent intent = new Intent("com.example.compat.TOGGLED");
            int before = pm.getComponentEnabledSetting(toggled);
            int found = pm.queryIntentActivities(intent, 0).size();
            pm.setComponentEnabledSetting(toggled, PackageManager.COMPONENT_ENABLED_STATE_DISABLED,
                    PackageManager.DONT_KILL_APP);
            System.out.println("packages toggled before " + before + " found " + found + " after "
                    + pm.getComponentEnabledSetting(toggled) + " found " + pm.queryIntentActivities(intent, 0).size());
            try {
                pm.setComponentEnabledSetting(new ComponentName("com.example.peer", "com.example.peer.PeerActivity"),
                        PackageManager.COMPONENT_ENABLED_STATE_DISABLED, PackageManager.DONT_KILL_APP);
                System.out.println("packages toggled another app changed");
            } catch (SecurityException e) {
                System.out.println("packages toggled another app refused");
            }
        });
        check("installs", () -> System.out.println("packages can request installs " + pm.canRequestPackageInstalls()));
    }
}
