package dev.uxdroid.system;

import android.graphics.Bitmap;
import android.os.Parcel;
import java.lang.reflect.Method;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.IntBuffer;

/**
 * Bitmaps in uxdroid's parcels, called by the natives in crates/system that replace Bitmap's
 * nativeWriteToParcel and nativeCreateFromParcel. AOSP's host library cannot reach a parcel that is
 * not its C++ one, so the pixels travel as unpremultiplied ARGB words in a blob, after a header of
 * mutability, config, width, height, density, premultiplication and alpha. Semi-transparent pixels
 * of a premultiplied bitmap can change by rounding on the way; an unpremultiplied one's do not.
 */
final class BitmapParcels {
    private BitmapParcels() {}

    /** Bitmap.Config's nativeInt values, in Android 16. */
    private static final int ALPHA_8 = 1, RGB_565 = 3, ARGB_4444 = 4, ARGB_8888 = 5, RGBA_F16 = 6,
            HARDWARE = 7, RGBA_1010102 = 8;

    private static Method copy;
    private static Method config;
    private static Method immutable;

    private static synchronized void methods() throws ReflectiveOperationException {
        if (copy != null) return;
        config = Bitmap.class.getDeclaredMethod("nativeConfig", long.class);
        immutable = Bitmap.class.getDeclaredMethod("nativeIsImmutable", long.class);
        Method m = Bitmap.class.getDeclaredMethod("nativeCopy", long.class, int.class, boolean.class);
        config.setAccessible(true);
        immutable.setAccessible(true);
        m.setAccessible(true);
        copy = m;
    }

    static boolean write(long bitmap, int density, Parcel parcel) throws ReflectiveOperationException {
        methods();
        int nativeConfig = (Integer) config.invoke(null, bitmap);
        boolean mutable = !(Boolean) immutable.invoke(null, bitmap);
        Bitmap pixels = (Bitmap) copy.invoke(null, bitmap, ARGB_8888, false);
        if (pixels == null) return false;
        // Everything is read before anything is written, so a failure leaves the parcel as it was.
        int width, height;
        boolean premultiplied, alpha;
        ByteBuffer bytes;
        try {
            width = pixels.getWidth();
            height = pixels.getHeight();
            premultiplied = pixels.isPremultiplied();
            alpha = pixels.hasAlpha();
            int[] colors = new int[width * height];
            pixels.getPixels(colors, 0, width, 0, 0, width, height);
            bytes = ByteBuffer.allocate(colors.length * 4).order(ByteOrder.LITTLE_ENDIAN);
            bytes.asIntBuffer().put(colors);
        } finally {
            pixels.recycle();
        }
        parcel.writeInt(mutable ? 1 : 0);
        parcel.writeInt(nativeConfig);
        parcel.writeInt(width);
        parcel.writeInt(height);
        parcel.writeInt(density);
        parcel.writeInt(premultiplied ? 1 : 0);
        parcel.writeInt(alpha ? 1 : 0);
        parcel.writeBlob(bytes.array());
        return true;
    }

    /** Reads what {@link #write} wrote. Every size is checked against the pixels the parcel holds. */
    static Bitmap read(Parcel parcel) {
        boolean mutable = parcel.readInt() != 0;
        int nativeConfig = parcel.readInt();
        int width = parcel.readInt(), height = parcel.readInt(), density = parcel.readInt();
        boolean premultiplied = parcel.readInt() != 0, alpha = parcel.readInt() != 0;
        byte[] bytes = parcel.readBlob();
        if (bytes == null || width <= 0 || height <= 0 || (long) width * height * 4 != bytes.length) {
            return null;
        }
        int[] colors = new int[width * height];
        IntBuffer words = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).asIntBuffer();
        words.get(colors);
        Bitmap bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888);
        // Before the pixels go in: setPixels premultiplies them into a premultiplied bitmap, and
        // changing that later only changes how the stored words are read.
        bitmap.setPremultiplied(premultiplied);
        bitmap.setPixels(colors, 0, width, 0, 0, width, height);
        Bitmap.Config wanted = config(nativeConfig);
        if (wanted != Bitmap.Config.ARGB_8888 || !mutable) {
            Bitmap converted = bitmap.copy(wanted, mutable);
            bitmap.recycle();
            if (converted == null) return null;
            bitmap = converted;
        }
        bitmap.setDensity(density);
        bitmap.setHasAlpha(alpha);
        if (bitmap.getConfig() == Bitmap.Config.RGBA_F16) bitmap.setPremultiplied(premultiplied);
        return bitmap;
    }

    /** The config to rebuild; a hardware bitmap comes back in memory, as no GPU buffer travels. */
    private static Bitmap.Config config(int nativeConfig) {
        switch (nativeConfig) {
            case ALPHA_8: return Bitmap.Config.ALPHA_8;
            case RGB_565: return Bitmap.Config.RGB_565;
            case ARGB_4444: return Bitmap.Config.ARGB_4444;
            case RGBA_F16: return Bitmap.Config.RGBA_F16;
            case RGBA_1010102: return Bitmap.Config.RGBA_1010102;
            case HARDWARE:
            default: return Bitmap.Config.ARGB_8888;
        }
    }
}
