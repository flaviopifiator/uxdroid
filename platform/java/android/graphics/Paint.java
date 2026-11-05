package android.graphics;

/** How to draw: color and text size. */
public class Paint {
    private int color = Color.BLACK;
    private float textSize = 16;

    public void setColor(int color) {
        this.color = color;
    }

    public int getColor() {
        return color;
    }

    public void setTextSize(float textSize) {
        this.textSize = textSize;
    }

    public float getTextSize() {
        return textSize;
    }
}
