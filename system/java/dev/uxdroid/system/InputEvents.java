package dev.uxdroid.system;

import android.view.KeyEvent;
import android.view.MotionEvent;

/** Makes the framework's input events from what a window's channel carries. Times are uptime ms. */
final class InputEvents {
    private InputEvents() {}

    static KeyEvent key(long downTime, long eventTime, int action, int code, int repeat, int metaState,
            int deviceId, int scanCode, int flags, int source) {
        return new KeyEvent(downTime, eventTime, action, code, repeat, metaState, deviceId, scanCode, flags,
                source);
    }

    /**
     * @param coords x, y and pressure of each pointer, in order
     * @param vscroll the scroll axes of the first pointer, for ACTION_SCROLL
     */
    static MotionEvent motion(long downTime, long eventTime, int action, int actionButton, int buttonState,
            int metaState, int source, int deviceId, int[] ids, int[] tools, float[] coords, float vscroll,
            float hscroll) {
        int count = ids.length;
        MotionEvent.PointerProperties[] properties = new MotionEvent.PointerProperties[count];
        MotionEvent.PointerCoords[] pointers = new MotionEvent.PointerCoords[count];
        for (int i = 0; i < count; i++) {
            properties[i] = new MotionEvent.PointerProperties();
            properties[i].id = ids[i];
            properties[i].toolType = tools[i];
            pointers[i] = new MotionEvent.PointerCoords();
            pointers[i].x = coords[3 * i];
            pointers[i].y = coords[3 * i + 1];
            pointers[i].pressure = coords[3 * i + 2];
            pointers[i].size = 1f;
        }
        pointers[0].setAxisValue(MotionEvent.AXIS_VSCROLL, vscroll);
        pointers[0].setAxisValue(MotionEvent.AXIS_HSCROLL, hscroll);
        MotionEvent event = MotionEvent.obtain(downTime, eventTime, action, count, properties, pointers, metaState,
                buttonState, 1f, 1f, deviceId, 0, source, 0);
        if (actionButton != 0) {
            event.setActionButton(actionButton);
        }
        return event;
    }
}
