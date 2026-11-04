package dev.uxdroid.system;

import android.content.pm.FeatureInfo;
import java.util.ArrayList;
import java.util.List;

/**
 * What the device has, as PackageManager.hasSystemFeature answers it: a desktop computer, as Android
 * describes ChromeOS and other PCs with android.hardware.type.pc, whose window takes touch, mouse
 * and keyboard from the Wayland seat, plays and records sound, and draws with the host's OpenGL ES.
 * It has no camera, telephony, sensors, NFC or WebView provider, so apps hide what needs them.
 */
final class Features {
    private Features() {}

    /** OpenGL ES 3.2, which the host's Mesa gives, also with llvmpipe. */
    static final int GL_ES_VERSION = 0x30002;

    private static final String[] NAMES = {
        "android.hardware.type.pc",
        "android.hardware.touchscreen",
        "android.hardware.touchscreen.multitouch",
        "android.hardware.faketouch",
        "android.hardware.screen.portrait",
        "android.hardware.screen.landscape",
        "android.hardware.audio.output",
        "android.hardware.microphone",
        "android.software.activities_on_secondary_displays",
    };

    static boolean has(String name, int version) {
        if (name == null) return false;
        for (String feature : NAMES) {
            if (feature.equals(name)) return version <= 0;
        }
        return false;
    }

    static List<FeatureInfo> list() {
        List<FeatureInfo> features = new ArrayList<>();
        for (String name : NAMES) {
            FeatureInfo feature = new FeatureInfo();
            feature.name = name;
            features.add(feature);
        }
        // The entry without a name holds the OpenGL ES version, as on Android.
        FeatureInfo gl = new FeatureInfo();
        gl.reqGlEsVersion = GL_ES_VERSION;
        features.add(gl);
        return features;
    }
}
