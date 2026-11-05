package dev.uxdroid.system;

import android.app.ContentProviderHolder;
import android.content.ComponentName;
import android.content.pm.PackageManager;
import android.content.pm.ProviderInfo;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;

/**
 * The app's own content providers, as Android's activity manager hands them out. At bind the app gets
 * the enabled providers of its process, in the order of their initOrder, and the framework installs
 * them before Application.onCreate, which is how androidx-startup's InitializationProvider runs. A
 * lookup of one of its own authorities answers with the provider's info and no provider, which makes
 * the framework create it in this process, as Android does for a provider that can run in the caller.
 *
 * <p>A provider declared for another process of the app has no process to run in here, as uxdroid runs
 * one process per app; it is created in the app's process when first looked up, and that is reported.
 */
final class Providers {
    private Providers() {}

    private static final Set<String> MOVED = ConcurrentHashMap.newKeySet();

    /** What ActivityManagerService's generateApplicationProviders gives the app's process. */
    static List<ProviderInfo> atBind(Launcher.App app) {
        List<ProviderInfo> found = new ArrayList<>();
        for (ProviderInfo info : app.providers()) {
            if (enabled(app, info) && inAppProcess(app, info)) found.add(info);
        }
        // As the package manager lists them for the bind: highest initOrder first, else manifest order.
        found.sort((a, b) -> Integer.compare(b.initOrder, a.initOrder));
        return found;
    }

    /** The holder of one of the app's authorities, with no provider, or null if the app has none. */
    static ContentProviderHolder holder(Launcher.App app, String authority) {
        ProviderInfo info = byAuthority(app, authority);
        if (info == null) return null;
        if (!inAppProcess(app, info) && MOVED.add(info.name)) {
            System.err.println("uxdroid: provider " + info.name + " of process " + info.processName
                    + " runs in the app's process " + app.application.processName);
        }
        ContentProviderHolder holder = new ContentProviderHolder(info);
        // Local: the framework makes the provider itself instead of waiting for another process to.
        holder.mLocal = true;
        holder.noReleaseNeeded = true;
        return holder;
    }

    /** PackageManager.getProviderInfo of one of the app's providers, enabled or not; null for others. */
    static ProviderInfo info(Launcher.App app, ComponentName component, long flags) {
        if (!component.getPackageName().equals(app.application.packageName)) return null;
        for (ProviderInfo info : app.providers()) {
            if (info.name.equals(component.getClassName())) return withFlags(info, flags);
        }
        return null;
    }

    /** PackageManager.resolveContentProvider: the enabled provider of an authority of the app. */
    static ProviderInfo resolve(Launcher.App app, String authority, long flags) {
        ProviderInfo info = byAuthority(app, authority);
        return info == null ? null : withFlags(info, flags);
    }

    private static ProviderInfo byAuthority(Launcher.App app, String authority) {
        if (authority == null) return null;
        for (ProviderInfo info : app.providers()) {
            if (info.authority == null || !enabled(app, info)) continue;
            for (String name : info.authority.split(";")) {
                if (name.equals(authority)) return info;
            }
        }
        return null;
    }

    /** Its manifest's state, or the one the app set since; see {@link EnabledSettings}. */
    private static boolean enabled(Launcher.App app, ProviderInfo info) {
        return EnabledSettings.enabled(app, info);
    }

    private static boolean inAppProcess(Launcher.App app, ProviderInfo info) {
        return info.processName == null || info.processName.equals(app.application.processName);
    }

    /** A copy holding only what the flags ask for, as the package manager answers. */
    private static ProviderInfo withFlags(ProviderInfo recorded, long flags) {
        ProviderInfo info = new ProviderInfo(recorded);
        if ((flags & PackageManager.GET_META_DATA) == 0) info.metaData = null;
        if ((flags & PackageManager.GET_URI_PERMISSION_PATTERNS) == 0) info.uriPermissionPatterns = null;
        return info;
    }
}
