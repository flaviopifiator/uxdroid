package dev.uxdroid.system;

/** Entry points of libuxdroid_system.so, the natives below AOSP's framework. */
final class Natives {
    private Natives() {}

    /** Gives every native in the install-time list a default body. Returns how many were bound. */
    static native int bindDefaults(String list);

    /** Loads the system properties, then binds this library's own natives over the defaults. */
    static native void bindSystem(String buildProp);
}
