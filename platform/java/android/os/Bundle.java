package android.os;

import java.util.HashMap;

/** The saved state handed to an Activity. */
public class Bundle {
    private final HashMap<String, Object> values = new HashMap<>();

    public void putString(String key, String value) {
        values.put(key, value);
    }

    public String getString(String key) {
        Object value = values.get(key);
        return value instanceof String ? (String) value : null;
    }

    public boolean containsKey(String key) {
        return values.containsKey(key);
    }
}
