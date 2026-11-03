package dev.uxdroid.system;

/**
 * System services the F-Droid corpus (runtime/framework/corpus_smoke.sh) needed beyond the ones the
 * launcher registers itself, each with the behavior those apps rely on rather than defaults.
 */
final class AppServices {
    private AppServices() {}

    static void register(Launcher.App app) {
        Services.register("connectivity", android.net.IConnectivityManager.class, new NetworkServices.Connectivity());
        Services.register("netstats", android.net.INetworkStatsService.class, new NetworkServices.Stats());
    }
}
