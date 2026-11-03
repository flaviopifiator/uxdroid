package dev.uxdroid.system;

import java.io.File;

/**
 * Brings up AOSP's framework in this process. Order matters: default bodies first, then AOSP's host
 * library replaces the natives it implements, then ours replace the rest we implement.
 */
public final class Boot {
    private Boot() {}

    /** Classes whose natives AOSP's host libandroid_runtime.so binds. Each one must be in its table. */
    private static final String[] CORE = {
        "android.animation.PropertyValuesHolder",
        "android.content.res.ApkAssets",
        "android.content.res.AssetManager",
        "android.content.res.StringBlock",
        "android.content.res.XmlBlock",
        "android.database.CursorWindow",
        "android.database.sqlite.SQLiteConnection",
        "android.database.sqlite.SQLiteDebug",
        "android.database.sqlite.SQLiteGlobal",
        "android.database.sqlite.SQLiteRawStatement",
        "android.media.ImageReader",
        "android.media.PublicFormatUtils",
        "android.os.FileObserver",
        "android.os.MessageQueue",
        "android.os.SystemProperties",
        "android.text.AndroidCharacter",
        "android.text.Hyphenator",
        "android.util.jar.StrictJarFile",
        "android.view.InputDevice",
        "android.view.KeyCharacterMap",
        "android.view.KeyEvent",
        "android.view.MotionEvent",
        "android.view.VelocityTracker",
        "com.android.internal.util.VirtualRefBasePtr",
    };

    private static final String[] GRAPHICS = {
        "android.graphics.Bitmap",
        "android.graphics.BitmapFactory",
        "android.graphics.BitmapRegionDecoder",
        "android.graphics.ByteBufferStreamAdaptor",
        "android.graphics.Camera",
        "android.graphics.Canvas",
        "android.graphics.CanvasProperty",
        "android.graphics.Color",
        "android.graphics.ColorFilter",
        "android.graphics.ColorSpace",
        "android.graphics.CreateJavaOutputStreamAdaptor",
        "android.graphics.DrawFilter",
        "android.graphics.FontFamily",
        "android.graphics.Gainmap",
        "android.graphics.Graphics",
        "android.graphics.HardwareBufferRenderer",
        "android.graphics.HardwareRenderer",
        "android.graphics.HardwareRendererObserver",
        "android.graphics.ImageDecoder",
        "android.graphics.Interpolator",
        "android.graphics.MaskFilter",
        "android.graphics.Matrix",
        "android.graphics.Mesh",
        "android.graphics.MeshSpecification",
        "android.graphics.NinePatch",
        "android.graphics.Paint",
        "android.graphics.Path",
        "android.graphics.PathEffect",
        "android.graphics.PathIterator",
        "android.graphics.PathMeasure",
        "android.graphics.Picture",
        "android.graphics.RecordingCanvas",
        "android.graphics.Region",
        "android.graphics.RenderEffect",
        "android.graphics.RenderNode",
        "android.graphics.RuntimeXfermode",
        "android.graphics.Shader",
        "android.graphics.Typeface",
        "android.graphics.YuvImage",
        "android.graphics.animation.NativeInterpolatorFactory",
        "android.graphics.animation.RenderNodeAnimator",
        "android.graphics.drawable.AnimatedImageDrawable",
        "android.graphics.drawable.AnimatedVectorDrawable",
        "android.graphics.drawable.VectorDrawable",
        "android.graphics.fonts.Font",
        "android.graphics.fonts.FontFamily",
        "android.graphics.text.GraphemeBreak",
        "android.graphics.text.LineBreaker",
        "android.graphics.text.MeasuredText",
        "android.graphics.text.TextRunShaper",
        "android.util.PathParser",
    };

    private static boolean started;
    private static int defaults = -1;
    private static String defaultsRoot;

    /**
     * @param root the runtime, laid out as Android's system partition
     * @param systemLibrary path of libuxdroid_system.so
     * @return how many natives got a default body
     */
    public static int start(String root, String systemLibrary) {
        return start(root, systemLibrary, null);
    }

    /**
     * @param defaultsLibrary the library of default bodies {@code uxdroid install} made from the same
     *     runtime's list, or null. ART binds its bodies by name when a native nobody registered is first
     *     called, so nothing is bound here; without it, every listed native gets a stub now.
     * @return how many natives have a default body
     */
    public static synchronized int start(String root, String systemLibrary, String defaultsLibrary) {
        if (started) {
            throw new IllegalStateException("the framework is already started");
        }
        System.load(systemLibrary);
        // Default bodies can be made once per process, so a start that failed later keeps them.
        if (defaults < 0) {
            String list = root + "/framework/natives.txt";
            if (defaultsLibrary != null) {
                String problem = Natives.defaultsProblem(defaultsLibrary);
                try {
                    if (problem == null) {
                        System.load(defaultsLibrary);
                        defaults = Natives.attachDefaults(defaultsLibrary, list);
                    }
                } catch (UnsatisfiedLinkError | IllegalStateException e) {
                    problem = e.getMessage();
                }
                if (problem != null) {
                    System.err.println("uxdroid: default bodies of " + defaultsLibrary + " not used, made now instead: "
                            + problem);
                }
            }
            if (defaults < 0) defaults = Natives.bindDefaults(list);
            defaultsRoot = root;
        } else if (!defaultsRoot.equals(root)) {
            throw new IllegalStateException("default bodies came from " + defaultsRoot + ", not " + root);
        }
        System.setProperty("use_base_native_hostruntime", "true");
        System.setProperty("core_native_classes", String.join(",", CORE));
        System.setProperty("graphics_native_classes", String.join(",", GRAPHICS));
        System.setProperty("icu.data.path", icuData(root));
        System.load(root + "/lib64/libandroid_runtime.so");
        Natives.bindSystem(root + "/build.prop");
        useDataDirectory();
        android.graphics.Typeface.loadPreinstalledSystemFontMap();
        started = true;
        return defaults;
    }

    /**
     * The framework names Android's data partition through Environment.getDataDirectory, which reads
     * ANDROID_DATA, except SQLite's default page size, the block size of a literal "/data": every
     * database an app opened failed with "Invalid path: /data". It is taken from ANDROID_DATA instead,
     * the file system the app's databases live on. A framework scan found no other literal an app
     * reaches; the rest belong to system_server.
     */
    private static void useDataDirectory() {
        File data = android.os.Environment.getDataDirectory();
        try {
            java.lang.reflect.Field pageSize = android.database.sqlite.SQLiteGlobal.class
                    .getDeclaredField("sDefaultPageSize");
            pageSize.setAccessible(true);
            pageSize.setInt(null, new android.os.StatFs(data.getPath()).getBlockSize());
        } catch (ReflectiveOperationException e) {
            throw new IllegalStateException("cannot give SQLite the page size of " + data, e);
        } catch (IllegalArgumentException e) {
            // No data directory, as for a program that only parses: it opens no database.
        }
    }

    private static String icuData(String root) {
        File[] found = new File(root, "usr/icu").listFiles((dir, name) -> name.startsWith("icudt") && name.endsWith(".dat"));
        if (found == null || found.length != 1) {
            throw new IllegalStateException("expected one ICU data file in " + root + "/usr/icu");
        }
        return found[0].getPath();
    }
}
