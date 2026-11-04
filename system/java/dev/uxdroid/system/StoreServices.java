package dev.uxdroid.system;

import android.content.pm.ParceledListSlice;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * System services app stores and microG asked for on start, beyond the ones the launcher and
 * {@link AppServices} register: a missing service makes the framework hand the app a null manager, and
 * a null list where the framework expects one stops it. Each answers with what is true here, often
 * nothing, until a later part of uxdroid gives it more: no accounts, no pending jobs, no install
 * sessions.
 */
final class StoreServices {
    private StoreServices() {}

    static void register(Launcher.App app) {
        Services.register("power", android.os.IPowerManager.class, new Power());
        // PowerManager is made only when the thermal service exists too.
        Services.register("thermalservice", android.os.IThermalService.class, new Thermal());
        Services.register("deviceidle", android.os.IDeviceIdleController.class, new DeviceIdle());
        Services.register("domain_verification",
                android.content.pm.verify.domain.IDomainVerificationManager.class, new DomainVerification(app));
        Services.register("jobscheduler", android.app.job.IJobScheduler.class, new Jobs());
        Services.register("account", android.accounts.IAccountManager.class, new Accounts());
    }

    /** What IPackageManager.getPackageInstaller returns; see {@link Sessions}. */
    static android.content.pm.IPackageInstaller packageInstaller() {
        return Services.local(android.content.pm.IPackageInstaller.class, new Sessions());
    }

    /**
     * The handler of IPackageInstaller. Apps install through {@code uxdroid install}; sessions, which
     * an app uses to install others, are not served yet, so there are none to list and creating one
     * fails naming the call.
     */
    public static final class Sessions {
        public ParceledListSlice getAllSessions(int userId) { return ParceledListSlice.emptyList(); }
        public ParceledListSlice getMySessions(String installerPackageName, int userId) {
            return ParceledListSlice.emptyList();
        }
        public ParceledListSlice getStagedSessions() { return ParceledListSlice.emptyList(); }
        /** No session exists, as Android answers for an id it does not know. */
        public android.content.pm.PackageInstaller.SessionInfo getSessionInfo(int sessionId) { return null; }
        /** With no sessions, no session event ever reaches a callback. */
        public void registerCallback(android.content.pm.IPackageInstallerCallback callback, int userId) {}
        public void unregisterCallback(android.content.pm.IPackageInstallerCallback callback) {}

        public int createSession(android.content.pm.PackageInstaller.SessionParams params, String installerPackageName,
                String installerAttributionTag, int userId) {
            throw new UnsupportedOperationException("IPackageInstaller.createSession is not implemented: "
                    + "uxdroid installs apps with uxdroid install, PackageInstaller sessions are not served yet");
        }
    }

    /**
     * The handler of IPowerManager: a desktop session in use, its screen on, not saving battery and not
     * idle. Wake locks are not held yet, so their calls are counted.
     */
    public static final class Power {
        public boolean isInteractive() { return true; }
        public boolean isDisplayInteractive(int displayId) { return true; }
        public boolean isPowerSaveMode() { return false; }
        public boolean isDeviceIdleMode() { return false; }
        public boolean isLightDeviceIdleMode() { return false; }
    }

    /**
     * The handler of IDeviceIdleController: a desktop has no doze and restricts no app in the
     * background, so every app is exempt from battery optimizations, as PowerManager asks.
     */
    public static final class DeviceIdle {
        public boolean isPowerSaveWhitelistApp(String packageName) { return true; }
        public boolean isPowerSaveWhitelistExceptIdleApp(String packageName) { return true; }
    }

    /**
     * The handler of IThermalService: no thermal status is read from the host yet, so it is none, the
     * status Android gives a device that is not throttling. A listener hears it once as it registers,
     * as ThermalManagerService tells it.
     */
    public static final class Thermal {
        public int getCurrentThermalStatus() { return android.os.Temperature.THROTTLING_NONE; }

        public boolean registerThermalStatusListener(android.os.IThermalStatusListener listener)
                throws android.os.RemoteException {
            listener.onStatusChange(android.os.Temperature.THROTTLING_NONE);
            return true;
        }

        public boolean unregisterThermalStatusListener(android.os.IThermalStatusListener listener) { return true; }
    }

    /**
     * The handler of IDomainVerificationManager. Nothing verifies web domains on a desktop, so every
     * web domain the app's links declare is in state none, and its link handling stays allowed, as
     * Android leaves it for an app nobody changed.
     */
    public static final class DomainVerification {
        private final Launcher.App app;

        DomainVerification(Launcher.App app) {
            this.app = app;
        }

        public android.content.pm.verify.domain.DomainVerificationUserState getDomainVerificationUserState(
                String packageName, int userId) {
            if (!app.application.packageName.equals(packageName)) {
                // DomainVerificationManager turns this code into NameNotFoundException.
                throw new android.os.ServiceSpecificException(
                        android.content.pm.verify.domain.DomainVerificationManager.INTERNAL_ERROR_NAME_NOT_FOUND,
                        packageName);
            }
            Map<String, Integer> hosts = new LinkedHashMap<>();
            for (String host : webDomains()) {
                hosts.put(host, android.content.pm.verify.domain.DomainVerificationUserState.DOMAIN_STATE_NONE);
            }
            return new android.content.pm.verify.domain.DomainVerificationUserState(
                    java.util.UUID.nameUUIDFromBytes(packageName.getBytes(java.nio.charset.StandardCharsets.UTF_8)),
                    packageName, android.os.UserHandle.of(userId), true, hosts);
        }

        /** Hosts of the activities' web links: VIEW, BROWSABLE and DEFAULT over http or https, as Android collects them. */
        private List<String> webDomains() {
            List<String> hosts = new ArrayList<>();
            for (android.content.pm.PackageParser.Activity activity : app.parsed().activities) {
                for (android.content.IntentFilter filter : activity.intents) {
                    boolean web = filter.hasAction(android.content.Intent.ACTION_VIEW)
                            && filter.hasCategory(android.content.Intent.CATEGORY_BROWSABLE)
                            && filter.hasCategory(android.content.Intent.CATEGORY_DEFAULT)
                            && (filter.hasDataScheme("http") || filter.hasDataScheme("https"));
                    for (int i = 0; web && i < filter.countDataAuthorities(); i++) {
                        String host = filter.getDataAuthority(i).getHost();
                        if (!hosts.contains(host)) hosts.add(host);
                    }
                }
            }
            return hosts;
        }
    }

    /**
     * The handler of IJobScheduler. Jobs do not run yet, so none is pending or started, and scheduling
     * one is counted and fails, as JobScheduler.RESULT_FAILURE.
     */
    public static final class Jobs {
        public Map<String, ParceledListSlice<android.app.job.JobInfo>> getAllPendingJobs() { return new HashMap<>(); }
        public ParceledListSlice<android.app.job.JobInfo> getAllPendingJobsInNamespace(String namespace) {
            return ParceledListSlice.emptyList();
        }
        /** No such job, as Android answers for an id it does not hold. */
        public android.app.job.JobInfo getPendingJob(String namespace, int jobId) { return null; }
        public List<android.app.job.JobInfo> getStartedJobs() { return new ArrayList<>(); }
        public ParceledListSlice getAllJobSnapshots() { return ParceledListSlice.emptyList(); }
        /** Nothing to cancel. */
        public void cancel(String namespace, int jobId) {}
        public void cancelAll() {}
        public void cancelAllInNamespace(String namespace) {}
    }

    /**
     * The handler of IAccountManager. No account and no authenticator exist yet, so every listing is
     * empty, and with no account ever changing, a listener never hears of one.
     */
    public static final class Accounts {
        private static final android.accounts.Account[] NONE = new android.accounts.Account[0];

        public android.accounts.Account[] getAccountsAsUser(String accountType, int userId, String opPackageName) {
            return NONE;
        }
        public android.accounts.Account[] getAccountsForPackage(String packageName, int uid, String opPackageName) {
            return NONE;
        }
        public android.accounts.Account[] getAccountsByTypeForPackage(String type, String packageName,
                String opPackageName) {
            return NONE;
        }
        public android.accounts.AuthenticatorDescription[] getAuthenticatorTypes(int userId) {
            return new android.accounts.AuthenticatorDescription[0];
        }
        public void registerAccountListener(String[] accountTypes, String opPackageName) {}
        public void unregisterAccountListener(String[] accountTypes, String opPackageName) {}
    }
}
