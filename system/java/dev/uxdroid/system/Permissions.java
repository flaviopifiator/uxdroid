package dev.uxdroid.system;

import java.util.Collection;
import java.util.Set;
import java.util.function.Supplier;

/**
 * The permissions the app's manifest requests. A desktop has no permission dialog, so a requested
 * permission counts as granted; one the manifest lacks is refused where uxdroid enforces it, such as
 * recording audio, as Android refuses it. The list is read on first use, so a start from the install
 * record parses the APK only when the app asks for something uxdroid guards.
 */
public final class Permissions {
    private static Supplier<? extends Collection<String>> source = Set::of;
    private static Set<String> requested;

    private Permissions() {}

    static synchronized void declare(Supplier<? extends Collection<String>> names) {
        source = names;
        requested = null;
    }

    /** Asked by libuxdroid's natives before they open a device for the app. */
    public static synchronized boolean requested(String permission) {
        if (requested == null) {
            Collection<String> names = source.get();
            requested = names == null ? Set.of() : Set.copyOf(names);
        }
        return requested.contains(permission);
    }
}
