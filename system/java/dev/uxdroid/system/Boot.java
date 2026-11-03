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
    public static synchronized int start(String root, String systemLibrary) {
        if (started) {
            throw new IllegalStateException("the framework is already started");
        }
        System.load(systemLibrary);
        // Default bodies can be made once per process, so a start that failed later keeps them.
        if (defaults < 0) {
            defaults = Natives.bindDefaults(root + "/framework/natives.txt");
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
        android.graphics.Typeface.loadPreinstalledSystemFontMap();
        started = true;
        return defaults;
    }

    private static String icuData(String root) {
        File[] found = new File(root, "usr/icu").listFiles((dir, name) -> name.startsWith("icudt") && name.endsWith(".dat"));
        if (found == null || found.length != 1) {
            throw new IllegalStateException("expected one ICU data file in " + root + "/usr/icu");
        }
        return found[0].getPath();
    }
}
