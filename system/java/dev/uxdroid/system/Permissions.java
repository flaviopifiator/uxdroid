package dev.uxdroid.system;

import java.util.Collection;
import java.util.Set;

/**
 * The permissions the app's manifest requests. A desktop has no permission dialog, so a requested
 * permission counts as granted; one the manifest lacks is refused where uxdroid enforces it, such as
 * recording audio, as Android refuses it.
 */
public final class Permissions {
    private static volatile Set<String> requested = Set.of();

    private Permissions() {}

    static void declare(Collection<String> names) {
        requested = Set.copyOf(names);
    }

    /** Asked by libuxdroid's natives before they open a device for the app. */
    public static boolean requested(String permission) {
        return requested.contains(permission);
    }
}
