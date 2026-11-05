package dev.uxdroid.system;

import android.content.ComponentName;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ApplicationInfo;
import android.content.pm.ComponentInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import java.io.File;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * Every app installed over the app's data directory, as the package manager knows them: the running
 * one, and each other {@code ANDROID_DATA/app/<package>/base.apk} from its install record, or its
 * parse when the record is missing or stale. An app is read again only when its identity, the runtime
 * and its APK files by size and modification time, changes, so an app installed while this one runs is
 * found on the next query.
 *
 * <p>Queries resolve intents over all of them, as Android's ComponentResolver does: a component the
 * intent names, else every enabled component one of whose filters matches, the components of other
 * apps only when exported. Package visibility (Android 11's {@code <queries>}) is not applied: every
 * installed app sees every other one.
 */
final class Packages {
    private Packages() {}

    /** The kinds of component a query resolves. */
    enum Kind { ACTIVITY, RECEIVER, SERVICE, PROVIDER }

    private static final class Known {
        final String identity;
        final Launcher.App app;

        Known(String identity, Launcher.App app) {
            this.identity = identity;
            this.app = app;
        }
    }

    private static String root;
    private static Launcher.App running;
    private static final Map<String, Known> KNOWN = new HashMap<>();
    private static final Set<String> REPORTED = new HashSet<>();

    /** The runtime the apps were installed for, and the app this process runs. */
    static synchronized void start(String runtimeRoot, Launcher.App app) {
        root = runtimeRoot;
        running = app;
    }

    static synchronized Launcher.App running() {
        return running;
    }

    /** The installed app of a package, or null. */
    static synchronized Launcher.App get(String packageName) {
        if (packageName == null) return null;
        if (running != null && packageName.equals(running.packageName())) return running;
        if (!valid(packageName) || root == null) return null;
        File apk = new File(Launcher.dataRoot(), "app/" + packageName + "/base.apk");
        if (!apk.isFile()) {
            KNOWN.remove(packageName);
            return null;
        }
        String identity = Record.identity(root, apk);
        Known known = KNOWN.get(packageName);
        if (known != null && known.identity.equals(identity)) return known.app;
        try {
            Launcher.App app = Launcher.App.installed(root, apk);
            if (!app.packageName().equals(packageName)) {
                throw new IllegalStateException("its manifest names the package " + app.packageName());
            }
            KNOWN.put(packageName, new Known(identity, app));
            return app;
        } catch (android.content.pm.PackageParser.PackageParserException | RuntimeException e) {
            if (REPORTED.add(packageName + identity)) {
                System.err.println("uxdroid: the installed app " + apk + " is not known to the package manager: " + e);
            }
            return null;
        }
    }

    /** Every installed app, by package name. */
    static synchronized List<Launcher.App> all() {
        List<Launcher.App> apps = new ArrayList<>();
        String[] names = new File(Launcher.dataRoot(), "app").list();
        if (names != null) {
            java.util.Arrays.sort(names);
            for (String name : names) {
                Launcher.App app = get(name);
                if (app != null) apps.add(app);
            }
        }
        if (running != null && !apps.contains(running)) apps.add(0, running);
        return apps;
    }

    /** A package name as uxdroid install accepts it, so no other directory is read. */
    private static boolean valid(String name) {
        return name.matches("[A-Za-z0-9_]+(\\.[A-Za-z0-9_]+)*");
    }

    private static ComponentInfo[] infos(Kind kind, Components components) {
        switch (kind) {
            case ACTIVITY: return components.activities;
            case RECEIVER: return components.receivers;
            case SERVICE: return components.services;
            default: return components.providers;
        }
    }

    private static IntentFilter[][] filters(Kind kind, Components components) {
        switch (kind) {
            case ACTIVITY: return components.activityFilters;
            case RECEIVER: return components.receiverFilters;
            case SERVICE: return components.serviceFilters;
            default: return components.providerFilters;
        }
    }

    /** A declared component of an installed app, as getActivityInfo and its kind answer; null if none. */
    static ComponentInfo info(Kind kind, ComponentName component, long flags) {
        Launcher.App app = get(component.getPackageName());
        if (app == null) return null;
        ComponentInfo info = Components.find(infos(kind, app.components()), component.getClassName());
        if (info == null || !visible(app, info, flags)) return null;
        return Components.withFlags(info, flags, app.applicationInfo(flags));
    }

    private static boolean visible(Launcher.App app, ComponentInfo info, long flags) {
        return (flags & PackageManager.MATCH_DISABLED_COMPONENTS) != 0 || EnabledSettings.enabled(app, info);
    }

    /**
     * queryIntentActivities and its kinds: the component the intent (or its selector) names, else every
     * enabled component whose filter matches, with MATCH_DEFAULT_ONLY only filters with the default
     * category, ordered as Android orders them: priority, then default, then how well the filter matched.
     */
    static List<ResolveInfo> query(Kind kind, Intent intent, String resolvedType, long flags) {
        if (intent.getComponent() == null && intent.getSelector() != null) intent = intent.getSelector();
        List<ResolveInfo> found = new ArrayList<>();
        ComponentName component = intent.getComponent();
        if (component != null) {
            Launcher.App app = get(component.getPackageName());
            ComponentInfo info = app == null ? null : Components.find(infos(kind, app.components()), component.getClassName());
            if (info != null && visible(app, info, flags)) found.add(resolveInfo(kind, app, info, null, 0, flags));
            return found;
        }
        String type = resolvedType != null ? resolvedType : intent.getType();
        boolean defaultOnly = (flags & PackageManager.MATCH_DEFAULT_ONLY) != 0;
        Launcher.App caller = running();
        List<Launcher.App> apps;
        if (intent.getPackage() != null) {
            Launcher.App only = get(intent.getPackage());
            apps = only == null ? List.of() : List.of(only);
        } else {
            apps = all();
        }
        for (Launcher.App app : apps) {
            Components components = app.components();
            ComponentInfo[] infos = infos(kind, components);
            IntentFilter[][] filters = filters(kind, components);
            for (int i = 0; i < infos.length; i++) {
                ComponentInfo info = infos[i];
                // As Android 12 and later, another app's component that is not exported is not offered.
                if ((app != caller && !info.exported) || !visible(app, info, flags)) continue;
                IntentFilter best = null;
                int bestMatch = -1;
                for (IntentFilter filter : filters[i]) {
                    if (defaultOnly && !filter.hasCategory(Intent.CATEGORY_DEFAULT)) continue;
                    int match = filter.match(intent.getAction(), type, intent.getScheme(), intent.getData(),
                            intent.getCategories(), "uxdroid");
                    if (match >= 0 && (best == null || filter.getPriority() > best.getPriority()
                            || filter.getPriority() == best.getPriority() && match > bestMatch)) {
                        best = filter;
                        bestMatch = match;
                    }
                }
                if (best != null) found.add(resolveInfo(kind, app, info, best, bestMatch, flags));
            }
        }
        found.sort((a, b) -> a.priority != b.priority ? Integer.compare(b.priority, a.priority)
                : a.isDefault != b.isDefault ? (a.isDefault ? -1 : 1) : Integer.compare(b.match, a.match));
        return found;
    }

    /**
     * resolveActivity and resolveService: the best of the query. Where Android would ask the user to
     * choose between equally good candidates, through its ResolverActivity, the first is taken.
     */
    static ResolveInfo resolve(Kind kind, Intent intent, String resolvedType, long flags) {
        List<ResolveInfo> found = query(kind, intent, resolvedType, flags);
        return found.isEmpty() ? null : found.get(0);
    }

    private static ResolveInfo resolveInfo(Kind kind, Launcher.App app, ComponentInfo declared, IntentFilter filter,
            int match, long flags) {
        ResolveInfo info = new ResolveInfo();
        ComponentInfo component = Components.withFlags(declared, flags, app.applicationInfo(flags));
        switch (kind) {
            case ACTIVITY: case RECEIVER: info.activityInfo = (android.content.pm.ActivityInfo) component; break;
            case SERVICE: info.serviceInfo = (android.content.pm.ServiceInfo) component; break;
            default: info.providerInfo = (android.content.pm.ProviderInfo) component; break;
        }
        if (filter != null) {
            info.priority = filter.getPriority();
            info.match = match;
            info.isDefault = filter.hasCategory(Intent.CATEGORY_DEFAULT);
            if ((flags & PackageManager.GET_RESOLVED_FILTER) != 0) info.filter = new IntentFilter(filter);
        }
        return info;
    }

    /**
     * The app's own startActivity of an intent none of its activities takes: if another installed app's
     * activity takes it, that app would have to start in its own process, which uxdroid cannot do from
     * inside an app yet, so this says so and fails with ActivityNotFoundException, which apps already
     * handle as nobody taking the intent. Returns normally when no other app takes it either.
     */
    static void refuseOtherApp(Intent intent, String resolvedType) {
        Launcher.App caller = running();
        ResolveInfo other = resolve(Kind.ACTIVITY, intent, resolvedType, PackageManager.MATCH_DEFAULT_ONLY);
        if (other == null || caller == null || other.activityInfo.packageName.equals(caller.packageName())) return;
        String target = new ComponentName(other.activityInfo.packageName, other.activityInfo.name).flattenToShortString();
        String message = "uxdroid cannot start " + target + " from " + caller.packageName()
                + ": it is another app, which runs in its own process, and starting one from an app is not"
                + " supported yet; start it with uxdroid app " + other.activityInfo.packageName + " start";
        System.err.println("uxdroid: " + message);
        throw new android.content.ActivityNotFoundException(message);
    }

    /** The app that has this application uid, or null. */
    static Launcher.App byUid(int uid) {
        for (Launcher.App app : all()) {
            if (app.application.uid == uid) return app;
        }
        return null;
    }

    static ApplicationInfo application(String packageName, long flags) {
        Launcher.App app = get(packageName);
        return app == null ? null : app.applicationInfo(flags);
    }
}
