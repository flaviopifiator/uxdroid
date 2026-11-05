package dev.uxdroid.system;

import android.content.IntentFilter;
import android.content.pm.ActivityInfo;
import android.content.pm.ApplicationInfo;
import android.content.pm.ComponentInfo;
import android.content.pm.PackageManager;
import android.content.pm.PackageParser;
import android.content.pm.ProviderInfo;
import android.content.pm.ServiceInfo;
import android.content.pm.pkg.FrameworkPackageUserState;
import java.util.ArrayList;
import java.util.List;

/**
 * Every component an app declares, as the package manager describes it with its meta-data, each with
 * its intent filters: what queries and lookups by component need, from the install record or from the
 * framework's parse of the APK. Each info's applicationInfo is the app's placed ApplicationInfo.
 */
final class Components {
    /** What a component holds, so a lookup can hand out a copy with only what its flags ask for. */
    static final int FLAGS = PackageManager.GET_META_DATA | PackageManager.GET_URI_PERMISSION_PATTERNS;

    final ActivityInfo[] activities;
    final IntentFilter[][] activityFilters;
    final ActivityInfo[] receivers;
    final IntentFilter[][] receiverFilters;
    final ServiceInfo[] services;
    final IntentFilter[][] serviceFilters;
    final ProviderInfo[] providers;
    final IntentFilter[][] providerFilters;

    Components(ActivityInfo[] activities, IntentFilter[][] activityFilters, ActivityInfo[] receivers,
            IntentFilter[][] receiverFilters, ServiceInfo[] services, IntentFilter[][] serviceFilters,
            ProviderInfo[] providers, IntentFilter[][] providerFilters) {
        this.activities = activities;
        this.activityFilters = activityFilters;
        this.receivers = receivers;
        this.receiverFilters = receiverFilters;
        this.services = services;
        this.serviceFilters = serviceFilters;
        this.providers = providers;
        this.providerFilters = providerFilters;
        if (activities.length != activityFilters.length || receivers.length != receiverFilters.length
                || services.length != serviceFilters.length || providers.length != providerFilters.length) {
            throw new IllegalArgumentException("components and their filters do not match");
        }
    }

    /** From the framework's parse of the manifest, for {@code application}. */
    static Components of(PackageParser.Package parsed, ApplicationInfo application) {
        int n = parsed.activities.size();
        ActivityInfo[] activities = new ActivityInfo[n];
        IntentFilter[][] activityFilters = new IntentFilter[n][];
        for (int i = 0; i < n; i++) {
            PackageParser.Activity declared = parsed.activities.get(i);
            activities[i] = place(PackageParser.generateActivityInfo(declared, FLAGS, FrameworkPackageUserState.DEFAULT, 0),
                    application);
            activityFilters[i] = filters(declared.intents);
        }
        n = parsed.receivers.size();
        ActivityInfo[] receivers = new ActivityInfo[n];
        IntentFilter[][] receiverFilters = new IntentFilter[n][];
        for (int i = 0; i < n; i++) {
            PackageParser.Activity declared = parsed.receivers.get(i);
            receivers[i] = place(PackageParser.generateActivityInfo(declared, FLAGS, FrameworkPackageUserState.DEFAULT, 0),
                    application);
            receiverFilters[i] = filters(declared.intents);
        }
        n = parsed.services.size();
        ServiceInfo[] services = new ServiceInfo[n];
        IntentFilter[][] serviceFilters = new IntentFilter[n][];
        for (int i = 0; i < n; i++) {
            PackageParser.Service declared = parsed.services.get(i);
            services[i] = place(PackageParser.generateServiceInfo(declared, FLAGS, FrameworkPackageUserState.DEFAULT, 0),
                    application);
            serviceFilters[i] = filters(declared.intents);
        }
        ProviderInfo[] providers = Launcher.App.providers(parsed, application);
        IntentFilter[][] providerFilters = new IntentFilter[providers.length][];
        for (int i = 0; i < providers.length; i++) providerFilters[i] = filters(parsed.providers.get(i).intents);
        return new Components(activities, activityFilters, receivers, receiverFilters, services, serviceFilters,
                providers, providerFilters);
    }

    /** The parser's filters as plain IntentFilters, without their link back to the parsed component. */
    private static IntentFilter[] filters(List<? extends IntentFilter> declared) {
        IntentFilter[] filters = new IntentFilter[declared.size()];
        for (int i = 0; i < filters.length; i++) filters[i] = new IntentFilter(declared.get(i));
        return filters;
    }

    /** A copy of a generated info, so the parser's shared object is never changed, for this application. */
    private static <T extends ComponentInfo> T place(T generated, ApplicationInfo application) {
        T info;
        if (generated instanceof ActivityInfo) info = (T) new ActivityInfo((ActivityInfo) generated);
        else info = (T) new ServiceInfo((ServiceInfo) generated);
        info.applicationInfo = application;
        return info;
    }

    /** The declared component of this class, or null. */
    static <T extends ComponentInfo> T find(T[] kind, String className) {
        for (T info : kind) {
            if (info.name.equals(className)) return info;
        }
        return null;
    }

    /** A copy of a component holding only what the flags ask for, as the package manager answers. */
    static <T extends ComponentInfo> T withFlags(T recorded, long flags, ApplicationInfo application) {
        ComponentInfo info;
        if (recorded instanceof ActivityInfo) info = new ActivityInfo((ActivityInfo) recorded);
        else if (recorded instanceof ServiceInfo) info = new ServiceInfo((ServiceInfo) recorded);
        else info = new ProviderInfo((ProviderInfo) recorded);
        if ((flags & PackageManager.GET_META_DATA) == 0) info.metaData = null;
        if (info instanceof ProviderInfo && (flags & PackageManager.GET_URI_PERMISSION_PATTERNS) == 0) {
            ((ProviderInfo) info).uriPermissionPatterns = null;
        }
        info.applicationInfo = application;
        return (T) info;
    }

    /** Copies of the enabled components of a kind, as PackageInfo lists them; null when there are none. */
    static <T extends ComponentInfo> List<T> listed(T[] kind, Launcher.App app, long flags, ApplicationInfo application) {
        List<T> found = new ArrayList<>();
        for (T info : kind) {
            if ((flags & PackageManager.MATCH_DISABLED_COMPONENTS) != 0 || EnabledSettings.enabled(app, info)) {
                found.add(withFlags(info, flags, application));
            }
        }
        return found.isEmpty() ? null : found;
    }
}
