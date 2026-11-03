package dev.uxdroid.system;

import android.view.InputDevice;
import android.view.KeyCharacterMap;

/**
 * The handler of IInputManager. Its devices are the Wayland seat's keyboard, mouse and touchscreen,
 * whose events reach windows through their input channels, and the virtual keyboard every Android
 * device has. Their key character maps all read the seat's keymap in libuxdroid_system.so.
 */
public final class InputService {
    /** Ids libuxdroid_system.so gives the seat's events. */
    static final int KEYBOARD = 1;
    static final int MOUSE = 2;
    static final int TOUCHSCREEN = 3;

    private final InputDevice[] devices = {
        device(KeyCharacterMap.VIRTUAL_KEYBOARD, "Virtual", InputDevice.SOURCE_KEYBOARD, false),
        device(KEYBOARD, "uxdroid keyboard", InputDevice.SOURCE_KEYBOARD, true),
        device(MOUSE, "uxdroid pointer", InputDevice.SOURCE_MOUSE, true),
        device(TOUCHSCREEN, "uxdroid touchscreen", InputDevice.SOURCE_TOUCHSCREEN, false),
    };

    private static InputDevice device(int id, String name, int sources, boolean external) {
        InputDevice.Builder builder = new InputDevice.Builder().setId(id).setName(name).setSources(sources)
                .setExternal(external).setKeyCharacterMap(KeyCharacterMap.obtainEmptyMap(id));
        if (sources == InputDevice.SOURCE_KEYBOARD) {
            builder.setKeyboardType(InputDevice.KEYBOARD_TYPE_ALPHABETIC);
        }
        return builder.build();
    }

    public int[] getInputDeviceIds() {
        int[] ids = new int[devices.length];
        for (int i = 0; i < devices.length; i++) ids[i] = devices[i].getId();
        return ids;
    }

    public InputDevice getInputDevice(int id) {
        for (InputDevice device : devices) {
            if (device.getId() == id) return device;
        }
        return null;
    }
}
