package dev.uxdroid.system;

import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkInfo;
import android.net.NetworkRequest;
import android.net.connectivity.android.net.netstats.StatsResult;
import android.net.connectivity.android.net.netstats.TrafficStatsRateLimitCacheConfig;
import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.util.List;

/**
 * The network the host has, as an app's ConnectivityManager and TrafficStats ask for it. The process
 * uses the host's own network stack, so these only report it: the interface of the host's default route
 * is the one active network, with internet; with no default route there is none. Every corpus app
 * looked both services up, and Glide, in Aegis, stops without a ConnectivityManager.
 *
 * <p>Not yet: changes of the network after a callback heard of it; the network is not probed, so it is
 * not reported as validated; and an app's own traffic is not counted apart from the host's.
 */
final class NetworkServices {
    private NetworkServices() {}

    /** Android's netId of the one network; any value above the reserved ones would do. */
    static final int NET_ID = 100;

    /** The interface of the host's default route, from /proc/net/route, or null when it has none. */
    static String defaultInterface() {
        try {
            for (String line : Files.readAllLines(new File("/proc/net/route").toPath())) {
                String[] fields = line.trim().split("\\s+");
                if (fields.length > 2 && fields[1].equals("00000000") && !fields[0].equals("Iface")) return fields[0];
            }
        } catch (IOException e) {
            // No routing table to read, as in a namespace without /proc: no network.
        }
        return null;
    }

    static boolean wireless(String iface) {
        return new File("/sys/class/net/" + iface + "/wireless").exists();
    }

    /** The handler of IConnectivityManager. */
    public static final class Connectivity {
        private int requests;

        private static Network active() {
            return defaultInterface() == null ? null : new Network(NET_ID);
        }

        private static boolean ours(Network network) {
            return network != null && network.getNetId() == NET_ID && defaultInterface() != null;
        }

        private static NetworkCapabilities capabilities() {
            String iface = defaultInterface();
            if (iface == null) return null;
            return new NetworkCapabilities.Builder()
                    .addTransportType(wireless(iface) ? NetworkCapabilities.TRANSPORT_WIFI
                            : NetworkCapabilities.TRANSPORT_ETHERNET)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_RESTRICTED)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_TRUSTED)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_ROAMING)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_CONGESTED)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_SUSPENDED)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_FOREGROUND)
                    // A NetworkRequest.Builder asks for these by default.
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VCN_MANAGED)
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_BANDWIDTH_CONSTRAINED)
                    .build();
        }

        private static NetworkInfo info() {
            String iface = defaultInterface();
            if (iface == null) return null;
            boolean wifi = wireless(iface);
            @SuppressWarnings("deprecation")
            NetworkInfo info = new NetworkInfo(wifi ? android.net.ConnectivityManager.TYPE_WIFI
                    : android.net.ConnectivityManager.TYPE_ETHERNET, 0, wifi ? "WIFI" : "ETHERNET", "");
            info.setDetailedState(NetworkInfo.DetailedState.CONNECTED, null, null);
            return info;
        }

        public Network getActiveNetwork() { return active(); }
        public Network getActiveNetworkForUid(int uid, boolean ignoreBlocked) { return active(); }
        public NetworkInfo getActiveNetworkInfo() { return info(); }
        public NetworkInfo getActiveNetworkInfoForUid(int uid, boolean ignoreBlocked) { return info(); }

        public NetworkInfo getNetworkInfo(int type) {
            NetworkInfo info = info();
            return info != null && info.getType() == type ? info : null;
        }

        public NetworkInfo getNetworkInfoForUid(Network network, int uid, boolean ignoreBlocked) {
            return ours(network) ? info() : null;
        }

        public Network[] getAllNetworks() {
            Network network = active();
            return network == null ? new Network[0] : new Network[] {network};
        }

        public NetworkCapabilities[] getDefaultNetworkCapabilitiesForUser(int userId, String callingPackage,
                String callingFeatureId) {
            NetworkCapabilities capabilities = capabilities();
            return capabilities == null ? new NetworkCapabilities[0] : new NetworkCapabilities[] {capabilities};
        }

        public NetworkCapabilities getNetworkCapabilities(Network network, String callingPackage,
                String callingFeatureId) {
            return ours(network) ? capabilities() : null;
        }

        public LinkProperties getLinkProperties(Network network) {
            if (!ours(network)) return null;
            LinkProperties properties = new LinkProperties();
            properties.setInterfaceName(defaultInterface());
            return properties;
        }

        public boolean isActiveNetworkMetered() { return false; }

        /**
         * The app's network callbacks, registerNetworkCallback, registerDefaultNetworkCallback and
         * requestNetwork alike: the request is kept, and if the host's network satisfies it, its
         * callback hears of it once, as ConnectivityService's first CALLBACK_AVAILABLE, which carries
         * the capabilities and link properties. The network never changes after, as nothing watches
         * the host's routes.
         */
        public synchronized NetworkRequest requestNetwork(int uid, NetworkCapabilities need, int type,
                android.os.Messenger messenger, int timeout, android.os.IBinder binder, int legacyType,
                int callbackFlags, String callingPackage, String callingAttributionTag, int declaredMethods) {
            NetworkRequest.Type[] types = NetworkRequest.Type.values();
            NetworkRequest request = new NetworkRequest(need != null ? need : new NetworkCapabilities(), legacyType,
                    ++requests, types[Math.max(0, Math.min(type, types.length - 1))]);
            available(request, messenger);
            return request;
        }

        public synchronized NetworkRequest listenForNetwork(NetworkCapabilities need, android.os.Messenger messenger,
                android.os.IBinder binder, int callbackFlags, String callingPackage, String callingAttributionTag,
                int declaredMethods) {
            NetworkRequest request = new NetworkRequest(need != null ? need : new NetworkCapabilities(),
                    android.net.ConnectivityManager.TYPE_NONE, ++requests, NetworkRequest.Type.LISTEN);
            available(request, messenger);
            return request;
        }

        /**
         * Tells {@code messenger} the network is available for {@code request}. The message is queued,
         * so ConnectivityManager has filed the callback under the request before it is handled.
         */
        private static void available(NetworkRequest request, android.os.Messenger messenger) {
            NetworkCapabilities capabilities = capabilities();
            if (messenger == null || capabilities == null) return;
            // A default request follows the default network, whatever it asked; others need a match.
            boolean wanted = request.type == NetworkRequest.Type.TRACK_DEFAULT
                    || request.type == NetworkRequest.Type.TRACK_SYSTEM_DEFAULT
                    || request.canBeSatisfiedBy(capabilities);
            if (!wanted) return;
            android.os.Bundle data = new android.os.Bundle();
            data.putParcelable(NetworkRequest.class.getSimpleName(), new NetworkRequest(request));
            data.putParcelable(Network.class.getSimpleName(), new Network(NET_ID));
            data.putParcelable(NetworkCapabilities.class.getSimpleName(), capabilities);
            LinkProperties properties = new LinkProperties();
            properties.setInterfaceName(defaultInterface());
            data.putParcelable(LinkProperties.class.getSimpleName(), properties);
            android.os.Message message = android.os.Message.obtain();
            message.what = android.net.ConnectivityManager.CALLBACK_AVAILABLE;
            message.arg1 = 0; // not blocked
            message.setData(data);
            try {
                messenger.send(message);
            } catch (android.os.RemoteException e) {
                // The app's own handler: it cannot be gone while the app runs.
            }
        }

        public void releaseNetworkRequest(NetworkRequest request) {}

        /** RESTRICT_BACKGROUND_STATUS_DISABLED: a desktop does not restrict data in the background. */
        public int getRestrictBackgroundStatusByCaller() { return 1; }
    }

    /** The handler of INetworkStatsService: the host's interface counters, from /proc/net/dev. */
    public static final class Stats {
        private static final long UNSUPPORTED = android.net.TrafficStats.UNSUPPORTED;

        /** Received and sent bytes and packets of one interface, or of all but loopback for null. */
        static StatsResult counters(String only) {
            long rxBytes = 0, rxPackets = 0, txBytes = 0, txPackets = 0;
            boolean found = false;
            try {
                List<String> lines = Files.readAllLines(new File("/proc/net/dev").toPath());
                for (String line : lines.subList(Math.min(2, lines.size()), lines.size())) {
                    int colon = line.indexOf(':');
                    if (colon < 0) continue;
                    String iface = line.substring(0, colon).trim();
                    if (only == null ? iface.equals("lo") : !iface.equals(only)) continue;
                    String[] f = line.substring(colon + 1).trim().split("\\s+");
                    if (f.length < 10) continue;
                    rxBytes += Long.parseLong(f[0]);
                    rxPackets += Long.parseLong(f[1]);
                    txBytes += Long.parseLong(f[8]);
                    txPackets += Long.parseLong(f[9]);
                    found = true;
                }
            } catch (IOException | NumberFormatException e) {
                found = false;
            }
            if (!found && only != null) return new StatsResult(UNSUPPORTED, UNSUPPORTED, UNSUPPORTED, UNSUPPORTED);
            return found ? new StatsResult(rxBytes, rxPackets, txBytes, txPackets) : new StatsResult(0, 0, 0, 0);
        }

        public StatsResult getTotalStats() { return counters(null); }
        public StatsResult getIfaceStats(String iface) { return counters(iface); }

        /** One app's traffic is not counted apart from the host's. */
        public StatsResult getUidStats(int uid) {
            return new StatsResult(UNSUPPORTED, UNSUPPORTED, UNSUPPORTED, UNSUPPORTED);
        }

        /** TrafficStats caches answers only when this enables it. */
        public TrafficStatsRateLimitCacheConfig getRateLimitCacheConfig() {
            return new TrafficStatsRateLimitCacheConfig(false, 0, 0);
        }
    }
}
