package dev.uxdroid.system;

import android.content.pm.ActivityInfo;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageParser;
import android.content.pm.pkg.FrameworkPackageUserState;
import java.io.File;

/**
 * The part of {@code uxdroid install} that needs the framework: it parses the manifest with the
 * framework's own parser and records the result, and writes the library of default bodies, so a start
 * does neither. Run on the staged APK, it records the paths the APK will have once installed.
 *
 * <p>Prints {@code launcher <class>} (or {@code launcher none}) and {@code defaults <count>}.
 */
public final class Installer {
    private Installer() {}

    /** The library of default bodies, beside the installed APK. */
    static final String DEFAULTS = "defaults.so";

    public static void main(String[] args) throws Exception {
        if (args.length != 4) {
            System.err.println("usage: Installer <runtime root> <libuxdroid_system.so> <staged apk> <installed apk>");
            System.exit(2);
        }
        String root = args[0];
        File staged = new File(args[2]).getAbsoluteFile();
        File installed = new File(args[3]).getAbsoluteFile();
        File directory = staged.getParentFile();
        Boot.start(root, args[1]);
        // The manifest parser asks which permissions an old target SDK implies.
        Services.register("permissionmgr", android.permission.IPermissionManager.class,
                new Launcher.PermissionManager());

        PackageParser.Package parsed = Launcher.App.parsePackage(staged);
        ApplicationInfo application = Launcher.App.applicationInfo(parsed, installed, false);
        ActivityInfo activity = Launcher.App.activity(parsed, application, null);
        PackageInfo info = PackageParser.generatePackageInfo(parsed, new int[0], 0, 0, 0,
                java.util.Collections.emptySet(), FrameworkPackageUserState.DEFAULT, 0);
        // The launcher gives the caller its own ApplicationInfo, so the record need not hold it twice.
        info.applicationInfo = null;
        // A rename keeps the size and modification time the identity names.
        File record = new File(directory, Record.FILE);
        try {
            Record.write(record, Record.identity(root, staged), application, activity, info);
        } catch (IllegalArgumentException e) {
            // Something the record cannot hold: the launcher parses the APK at each start instead.
            record.delete();
            System.err.println("uxdroid: no install record, the APK is parsed at each start: " + e.getMessage());
        }
        int defaults = Natives.writeDefaults(root + "/framework/natives.txt", new File(directory, DEFAULTS).getPath());

        System.out.println("launcher " + (activity == null ? "none" : activity.name));
        System.out.println("defaults " + defaults);
        System.out.flush();
        // The framework may have started threads that would keep the process alive.
        System.exit(0);
    }
}
