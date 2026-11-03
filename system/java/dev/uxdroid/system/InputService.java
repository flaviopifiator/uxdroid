package dev.uxdroid.system;

import android.view.InputDevice;
import android.view.KeyCharacterMap;

/**
 * The handler of IInputManager. Input does not reach the app yet, but every Android device has the
 * virtual keyboard that key events without a device come from, and windows ask for its key map.
 */
public final class InputService {
    private final InputDevice virtualKeyboard = new InputDevice.Builder()
            .setId(KeyCharacterMap.VIRTUAL_KEYBOARD)
            .setName("Virtual")
            .setSources(InputDevice.SOURCE_KEYBOARD)
            .setKeyboardType(InputDevice.KEYBOARD_TYPE_ALPHABETIC)
            .setKeyCharacterMap(KeyCharacterMap.obtainEmptyMap(KeyCharacterMap.VIRTUAL_KEYBOARD))
            .build();

    public int[] getInputDeviceIds() {
        return new int[] {virtualKeyboard.getId()};
    }

    public InputDevice getInputDevice(int id) {
        return id == virtualKeyboard.getId() ? virtualKeyboard : null;
    }
}
