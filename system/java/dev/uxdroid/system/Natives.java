package dev.uxdroid.system;

/** Entry points of libuxdroid_system.so, the natives below AOSP's framework. */
final class Natives {
    private Natives() {}

    /** Gives every native in the install-time list a default body. Returns how many were bound. */
    static native int bindDefaults(String list);

    /** Writes the library of default bodies for the natives in the list, at install time. Returns how many. */
    static native int writeDefaults(String list, String library);

    /** Why a library from writeDefaults cannot be loaded as code, such as a noexec mount, or null. */
    static native String defaultsProblem(String library);

    /** Connects a library from writeDefaults, already loaded with System.load, to this one. Returns its size. */
    static native int attachDefaults(String library, String list);

    /** Loads the system properties, then binds this library's own natives over the defaults. */
    static native void bindSystem(String buildProp);

    /** The Android uid of an installed package, one of the application uids, stable for its name. */
    static native int applicationUid(String packageName);

    /**
     * From now on the framework sees this process as the package: Binder.getCallingUid and so
     * Process.myUid answer its application uid. The Linux uid does not change. Returns the uid.
     */
    static native int becomeApplication(String packageName);

    /** The input device id the seat's events carry: keyboard 0, mouse 1, touchscreen 2. */
    static native int seatDevice(int kind);
}
