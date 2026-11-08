import android.content.res.AssetManager;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.util.DisplayMetrics;

/** Starts AOSP's framework and uses its drawing, text and resources. Prints one line per check. */
public class FrameworkSmoke {
    public static void main(String[] args) {
        int defaults = dev.uxdroid.system.Boot.start(args[0], args[1]);
        System.out.println("defaults " + defaults);

        Bitmap bitmap = Bitmap.createBitmap(64, 32, Bitmap.Config.ARGB_8888);
        Canvas canvas = new Canvas(bitmap);
        Paint paint = new Paint();
        paint.setColor(0xff00c000);
        canvas.drawRect(4, 4, 20, 20, paint);
        System.out.printf("rect %08x %08x%n", bitmap.getPixel(10, 10), bitmap.getPixel(30, 10));
        decodeFromFile(bitmap);

        Bitmap text = Bitmap.createBitmap(64, 32, Bitmap.Config.ARGB_8888);
        paint.setColor(0xff000000);
        paint.setTextSize(20);
        new Canvas(text).drawText("Hola", 2, 24, paint);
        int inked = 0;
        for (int y = 0; y < text.getHeight(); y++) {
            for (int x = 0; x < text.getWidth(); x++) {
                if (text.getPixel(x, y) != 0) inked++;
            }
        }
        System.out.println("text " + inked);

        DisplayMetrics metrics = new DisplayMetrics();
        metrics.setToDefaults();
        Configuration config = new Configuration();
        config.setToDefaults();
        Resources resources = new Resources(AssetManager.getSystem(), metrics, config);
        System.out.println("string " + resources.getString(android.R.string.ok));
        System.out.println("sdk " + android.os.Build.VERSION.SDK_INT + " abi " + android.os.Build.SUPPORTED_ABIS[0]);
        android.util.Log.i("FrameworkSmoke", "log reaches stderr");

        android.os.Looper.prepareMainLooper();
        android.content.Context context = dev.uxdroid.system.AppContext.forFramework("dev.uxdroid.smoke");
        android.widget.TextView view = new android.widget.TextView(context);
        view.setText("Hola desde el framework real");
        view.setTextSize(20);
        int spec = android.view.View.MeasureSpec.makeMeasureSpec(400, android.view.View.MeasureSpec.AT_MOST);
        view.measure(spec, spec);
        view.layout(0, 0, view.getMeasuredWidth(), view.getMeasuredHeight());
        Bitmap drawn = Bitmap.createBitmap(view.getMeasuredWidth(), view.getMeasuredHeight(), Bitmap.Config.ARGB_8888);
        view.draw(new Canvas(drawn));
        System.out.println("view " + view.getMeasuredWidth() + "x" + view.getMeasuredHeight() + " " + inked(drawn));
        System.out.println("unhandled " + dev.uxdroid.system.Services.unhandled());
    }

    /**
     * Decodes a PNG file through the host library's natives that take a FileDescriptor, as image
     * loaders such as Coil do: they read the descriptor's int, which aborted the process once.
     */
    static void decodeFromFile(Bitmap bitmap) {
        java.io.File png = new java.io.File(System.getenv("ANDROID_DATA"), "decode.png");
        try {
            try (java.io.FileOutputStream out = new java.io.FileOutputStream(png)) {
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, out);
            }
            Bitmap decoded = android.graphics.ImageDecoder.decodeBitmap(
                    android.graphics.ImageDecoder.createSource(png),
                    (decoder, info, source) -> decoder.setAllocator(android.graphics.ImageDecoder.ALLOCATOR_SOFTWARE));
            System.out.printf("imagedecoder %dx%d %08x%n", decoded.getWidth(), decoded.getHeight(), decoded.getPixel(10, 10));
            try (java.io.FileInputStream in = new java.io.FileInputStream(png)) {
                Bitmap fromFd = android.graphics.BitmapFactory.decodeFileDescriptor(in.getFD());
                System.out.printf("bitmapfactory %dx%d %08x%n", fromFd.getWidth(), fromFd.getHeight(), fromFd.getPixel(10, 10));
            }
            java.nio.ByteBuffer pixels = java.nio.ByteBuffer.allocateDirect(bitmap.getByteCount());
            bitmap.copyPixelsToBuffer(pixels);
            System.out.printf("niobuffer %d %08x%n", pixels.position(), pixels.order(java.nio.ByteOrder.LITTLE_ENDIAN).getInt((10 * 64 + 10) * 4));
        } catch (java.io.IOException e) {
            System.out.println("decode failed: " + e);
        }
    }

    static int inked(Bitmap bitmap) {
        int count = 0;
        for (int y = 0; y < bitmap.getHeight(); y++) {
            for (int x = 0; x < bitmap.getWidth(); x++) {
                if (android.graphics.Color.alpha(bitmap.getPixel(x, y)) > 96) count++;
            }
        }
        return count;
    }
}
