package dev.uxdroid.system;

import android.content.pm.ComponentInfo;
import android.content.pm.PackageManager;
import java.io.File;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.util.HashMap;
import java.util.Map;

/**
 * Component enabled settings, as PackageManager.setComponentEnabledSetting changes them: kept per
 * package in {@code ANDROID_DATA/system/components/<package>}, one {@code <state> <class>} line each,
 * so they last across starts and updates, as Android keeps them in package-restrictions.xml. Every app
 * reads any package's settings; only the app itself changes its own, since the permission to change
 * another's is the system's. A file is replaced by a rename, so a reader never sees half of one.
 */
final class EnabledSettings {
    private EnabledSettings() {}

    /** Settings read from a file, with the modification time and size they were read at. */
    private static final class Read {
        final long modified;
        final long length;
        final Map<String, Integer> states;

        Read(long modified, long length, Map<String, Integer> states) {
            this.modified = modified;
            this.length = length;
            this.states = states;
        }
    }

    private static final Map<String, Read> CACHE = new HashMap<>();

    static File file(String packageName) {
        return new File(Launcher.dataRoot(), "system/components/" + packageName);
    }

    /** The state set for a component, COMPONENT_ENABLED_STATE_DEFAULT if none. */
    static synchronized int get(String packageName, String className) {
        Integer state = states(packageName).get(className);
        return state == null ? PackageManager.COMPONENT_ENABLED_STATE_DEFAULT : state;
    }

    /** Whether a component of an installed app is enabled: its setting, else its manifest. */
    static boolean enabled(Launcher.App app, ComponentInfo info) {
        if (!app.application.enabled) return false;
        switch (get(app.application.packageName, info.name)) {
            case PackageManager.COMPONENT_ENABLED_STATE_ENABLED: return true;
            case PackageManager.COMPONENT_ENABLED_STATE_DEFAULT: return info.enabled;
            default: return false;
        }
    }

    /** Sets a component's state and writes the package's file. */
    static synchronized void set(String packageName, String className, int state) {
        if (state < PackageManager.COMPONENT_ENABLED_STATE_DEFAULT
                || state > PackageManager.COMPONENT_ENABLED_STATE_DISABLED_UNTIL_USED) {
            throw new IllegalArgumentException("Invalid new component state: " + state);
        }
        Map<String, Integer> states = new HashMap<>(states(packageName));
        if (state == PackageManager.COMPONENT_ENABLED_STATE_DEFAULT) states.remove(className);
        else states.put(className, state);
        File file = file(packageName);
        StringBuilder text = new StringBuilder();
        states.entrySet().stream().sorted(Map.Entry.comparingByKey())
                .forEach(entry -> text.append(entry.getValue()).append(' ').append(entry.getKey()).append('\n'));
        try {
            Files.createDirectories(file.getParentFile().toPath());
            File staged = new File(file.getParentFile(), "." + packageName + "." + android.os.Process.myPid());
            Files.write(staged.toPath(), text.toString().getBytes(StandardCharsets.UTF_8));
            Files.move(staged.toPath(), file.toPath(), StandardCopyOption.ATOMIC_MOVE,
                    StandardCopyOption.REPLACE_EXISTING);
        } catch (IOException e) {
            throw new IllegalStateException("cannot write the component settings " + file + ": " + e.getMessage(), e);
        }
        CACHE.remove(packageName);
    }

    /** A package's settings, read again only when its file changed. */
    private static Map<String, Integer> states(String packageName) {
        File file = file(packageName);
        long modified = file.lastModified();
        long length = file.length();
        Read cached = CACHE.get(packageName);
        if (cached != null && cached.modified == modified && cached.length == length) return cached.states;
        Map<String, Integer> states = new HashMap<>();
        if (file.isFile()) {
            try {
                for (String line : Files.readAllLines(file.toPath(), StandardCharsets.UTF_8)) {
                    int space = line.indexOf(' ');
                    if (space <= 0) continue;
                    try {
                        states.put(line.substring(space + 1), Integer.parseInt(line.substring(0, space)));
                    } catch (NumberFormatException e) {
                        // A line nobody wrote is ignored, as a missing setting.
                    }
                }
            } catch (IOException e) {
                System.err.println("uxdroid: cannot read the component settings " + file + ": " + e.getMessage());
            }
        }
        CACHE.put(packageName, new Read(modified, length, states));
        return states;
    }
}
