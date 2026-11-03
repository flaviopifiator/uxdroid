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
    }
}
