package android.graphics;

/** Colors packed as ARGB. */
public class Color {
    public static final int BLACK = 0xFF000000;
    public static final int WHITE = 0xFFFFFFFF;
    public static final int RED = 0xFFFF0000;
    public static final int GREEN = 0xFF00FF00;
    public static final int BLUE = 0xFF0000FF;
    public static final int TRANSPARENT = 0;

    public static int rgb(int red, int green, int blue) {
        return argb(255, red, green, blue);
    }

    public static int argb(int alpha, int red, int green, int blue) {
        return (alpha << 24) | (red << 16) | (green << 8) | blue;
    }
}
