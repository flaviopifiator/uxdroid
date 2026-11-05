package dev.uxdroid.system;

import android.os.Bundle;
import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.File;
import java.io.IOException;
import java.lang.reflect.Array;
import java.lang.reflect.Field;
import java.lang.reflect.Modifier;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.IdentityHashMap;
import java.util.List;
import java.util.Map;

/**
 * The install record: what the framework's own manifest parser made of an APK, kept so that a start
 * reads it back instead of parsing the APK again. Objects are written field by field, as the framework
 * holds them, so the app sees exactly what the parser gave; a record is valid only for the runtime and
 * the APK file it was made with, which {@link #identity} names.
 */
final class Record {
    private Record() {}

    static final String FILE = "package.record";
    private static final String MAGIC = "uxdroid package record 3";
    /**
     * Bounds of a record another app may have left, checked before anything is allocated: microG's,
     * the largest seen, is 157 KB, and a framework object nests a few values deep. Past either, the
     * record is refused and the APK parsed.
     */
    static final int MAX_BYTES = 4 << 20;
    static final int MAX_DEPTH = 64;
    /** Files of the runtime a record depends on: the classes it holds and the natives its library lists. */
    private static final String[] RUNTIME_FILES = {
        "framework/framework.jar", "framework/uxdroid-system.jar", "framework/natives.txt",
    };

    /** What a start needs from the parsed manifest. */
    static final class Contents {
        final android.content.pm.ApplicationInfo application;
        final android.content.pm.ActivityInfo activity;
        /** The application's meta-data, which getApplicationInfo gives with GET_META_DATA; null if none. */
        final Bundle metaData;
        /** Every component the manifest declares, with its meta-data and intent filters. */
        final Components components;
        /** PackageManager.getPackageInfo with flags 0, decoded again for each caller. */
        final byte[] packageInfo;

        Contents(android.content.pm.ApplicationInfo application, android.content.pm.ActivityInfo activity,
                Bundle metaData, Components components, byte[] packageInfo) {
            this.application = application;
            this.activity = activity;
            this.metaData = metaData;
            this.components = components;
            this.packageInfo = packageInfo;
        }

        android.content.pm.PackageInfo packageInfo() {
            try {
                return (android.content.pm.PackageInfo) new Reader(packageInfo).value();
            } catch (IOException | ReflectiveOperationException e) {
                throw new IllegalStateException("cannot read the recorded package", e);
            }
        }
    }

    /**
     * The runtime, the APK and its splits, by path, size and modification time. Changing any makes a
     * record stale. An APK without splits has the identity it always had, so its record stays valid.
     */
    static String identity(String root, File apk) {
        StringBuilder text = new StringBuilder(new File(root).getAbsolutePath());
        for (String name : RUNTIME_FILES) {
            File file = new File(root, name);
            text.append('|').append(name).append(' ').append(file.length()).append(' ').append(file.lastModified());
        }
        text.append("|apk ").append(apk.length()).append(' ').append(apk.lastModified());
        for (String name : Launcher.App.splits(apk)) {
            File split = Launcher.App.split(apk, name);
            text.append("|split ").append(name).append(' ').append(split.length()).append(' ')
                    .append(split.lastModified());
        }
        return text.toString();
    }

    /**
     * Writes a record. Components whose applicationInfo is {@code application} hold a reference to it,
     * so a read gives them back the very ApplicationInfo the launcher places; the launcher activity is
     * one of the components, so it is the same object as its entry there.
     */
    static void write(File file, String identity, android.content.pm.ApplicationInfo application,
            android.content.pm.ActivityInfo activity, Bundle metaData, Components components,
            android.content.pm.PackageInfo packageInfo) throws IOException, ReflectiveOperationException {
        Writer info = new Writer();
        info.value(packageInfo);
        Writer out = new Writer();
        out.data.writeUTF(MAGIC);
        out.data.writeUTF(identity);
        out.value(application);
        out.value(activity);
        out.value(metaData);
        out.value(components.activities);
        out.value(components.activityFilters);
        out.value(components.receivers);
        out.value(components.receiverFilters);
        out.value(components.services);
        out.value(components.serviceFilters);
        out.value(components.providers);
        out.value(components.providerFilters);
        out.bytes(info.toByteArray());
        byte[] record = out.toByteArray();
        if (record.length > MAX_BYTES) {
            throw new IllegalArgumentException("the record would be larger than " + MAX_BYTES + " bytes");
        }
        java.nio.file.Files.write(file.toPath(), record);
    }

    /** Whether a record was made for this identity, reading only its header: no framework class is touched. */
    static boolean matches(File file, String identity) {
        try (DataInputStream in = new DataInputStream(new java.io.BufferedInputStream(
                new java.io.FileInputStream(file), 4096))) {
            return MAGIC.equals(in.readUTF()) && identity.equals(in.readUTF());
        } catch (IOException e) {
            return false;
        }
    }

    /** Reads a record, or explains in the exception why it cannot be used. */
    static Contents read(File file, String identity) throws IOException, ReflectiveOperationException {
        Reader in = new Reader(bounded(file));
        if (!MAGIC.equals(in.data.readUTF())) throw new IOException("not a record of this version");
        if (!identity.equals(in.data.readUTF())) throw new IOException("the runtime or the APK changed since install");
        android.content.pm.ApplicationInfo application = (android.content.pm.ApplicationInfo) in.value();
        android.content.pm.ActivityInfo activity = (android.content.pm.ActivityInfo) in.value();
        Bundle metaData = (Bundle) in.value();
        Components components = new Components((android.content.pm.ActivityInfo[]) in.value(),
                (android.content.IntentFilter[][]) in.value(), (android.content.pm.ActivityInfo[]) in.value(),
                (android.content.IntentFilter[][]) in.value(), (android.content.pm.ServiceInfo[]) in.value(),
                (android.content.IntentFilter[][]) in.value(), (android.content.pm.ProviderInfo[]) in.value(),
                (android.content.IntentFilter[][]) in.value());
        return new Contents(application, activity, metaData, components, in.bytes());
    }

    private static final byte NULL = 0, REFERENCE = 1, OBJECT = 2, STRING = 3, ARRAY = 4, BUNDLE = 5, LIST = 6,
            BOOLEAN = 7, BYTE = 8, CHAR = 9, SHORT = 10, INT = 11, LONG = 12, FLOAT = 13, DOUBLE = 14, UUID = 15,
            SET = 16;

    /** Instance fields of a class and its superclasses, in a fixed order. */
    private static List<Field> fields(Class<?> type) {
        List<Field> fields = new ArrayList<>();
        for (Class<?> c = type; c != null && c != Object.class; c = c.getSuperclass()) {
            for (Field field : c.getDeclaredFields()) {
                if (Modifier.isStatic(field.getModifiers())) continue;
                field.setAccessible(true);
                fields.add(field);
            }
        }
        return fields;
    }

    private static final class Writer {
        final ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        final DataOutputStream data = new DataOutputStream(bytes);
        final Map<Object, Integer> seen = new IdentityHashMap<>();

        byte[] toByteArray() throws IOException {
            data.flush();
            return bytes.toByteArray();
        }

        void bytes(byte[] value) throws IOException {
            data.writeInt(value.length);
            data.write(value);
        }

        void string(String value) throws IOException {
            bytes(value.getBytes(StandardCharsets.UTF_8));
        }

        void value(Object value) throws IOException, ReflectiveOperationException {
            if (value == null) {
                data.writeByte(NULL);
            } else if (value instanceof String) {
                data.writeByte(STRING);
                string((String) value);
            } else if (value instanceof Boolean) {
                data.writeByte(BOOLEAN);
                data.writeBoolean((Boolean) value);
            } else if (value instanceof Byte) {
                data.writeByte(BYTE);
                data.writeByte((Byte) value);
            } else if (value instanceof Character) {
                data.writeByte(CHAR);
                data.writeChar((Character) value);
            } else if (value instanceof Short) {
                data.writeByte(SHORT);
                data.writeShort((Short) value);
            } else if (value instanceof Integer) {
                data.writeByte(INT);
                data.writeInt((Integer) value);
            } else if (value instanceof Long) {
                data.writeByte(LONG);
                data.writeLong((Long) value);
            } else if (value instanceof Float) {
                data.writeByte(FLOAT);
                data.writeFloat((Float) value);
            } else if (value instanceof Double) {
                data.writeByte(DOUBLE);
                data.writeDouble((Double) value);
            } else if (value instanceof java.util.UUID) {
                data.writeByte(UUID);
                string(value.toString());
            } else if (seen.containsKey(value)) {
                data.writeByte(REFERENCE);
                data.writeInt(seen.get(value));
            } else {
                seen.put(value, seen.size());
                Class<?> type = value.getClass();
                if (type.isArray()) {
                    Class<?> component = type.getComponentType();
                    Class<?> element = component;
                    while (element.isArray()) element = element.getComponentType();
                    if (!element.isPrimitive() && element != String.class && !element.getName().startsWith("android.")) {
                        throw new IllegalArgumentException("cannot record an array of " + component.getName());
                    }
                    data.writeByte(ARRAY);
                    string(component.getName());
                    int length = Array.getLength(value);
                    data.writeInt(length);
                    for (int i = 0; i < length; i++) value(Array.get(value, i));
                } else if (type == Bundle.class) {
                    Bundle bundle = (Bundle) value;
                    data.writeByte(BUNDLE);
                    data.writeInt(bundle.size());
                    for (String key : bundle.keySet()) {
                        Object element = bundle.get(key);
                        if (element != null && !(element instanceof String || element instanceof Integer
                                || element instanceof Boolean || element instanceof Float || element instanceof Long
                                || element instanceof Double)) {
                            throw new IllegalArgumentException("cannot record meta-data of type "
                                    + element.getClass().getName());
                        }
                        string(key);
                        value(element);
                    }
                } else if (type == ArrayList.class) {
                    List<?> list = (List<?>) value;
                    data.writeByte(LIST);
                    data.writeInt(list.size());
                    for (Object element : list) value(element);
                } else if (type == android.util.ArraySet.class) {
                    // An IntentFilter's actions: its elements, since its own arrays hold Objects.
                    android.util.ArraySet<?> set = (android.util.ArraySet<?>) value;
                    data.writeByte(SET);
                    data.writeInt(set.size());
                    for (Object element : set) value(element);
                } else if (type.getName().startsWith("android.")) {
                    data.writeByte(OBJECT);
                    string(type.getName());
                    for (Field field : fields(type)) value(field.get(value));
                } else {
                    throw new IllegalArgumentException("cannot record a " + type.getName());
                }
            }
        }
    }

    /** The record's bytes, refused before they are read when there are more than a record holds. */
    private static byte[] bounded(File file) throws IOException {
        String tooLarge = "the record is larger than " + MAX_BYTES + " bytes";
        if (file.length() > MAX_BYTES) throw new IOException(tooLarge);
        try (java.io.InputStream in = new java.io.FileInputStream(file)) {
            // The file may have grown since its length was read.
            byte[] bytes = in.readNBytes(MAX_BYTES + 1);
            if (bytes.length > MAX_BYTES) throw new IOException(tooLarge);
            return bytes;
        }
    }

    private static final class Reader {
        final DataInputStream data;
        final List<Object> seen = new ArrayList<>();
        private int depth;

        Reader(byte[] bytes) {
            data = new DataInputStream(new ByteArrayInputStream(bytes));
        }

        byte[] bytes() throws IOException {
            int length = data.readInt();
            if (length < 0 || length > data.available()) throw new IOException("truncated record");
            byte[] value = new byte[length];
            data.readFully(value);
            return value;
        }

        String string() throws IOException {
            return new String(bytes(), StandardCharsets.UTF_8);
        }

        /** One value, refused when it nests deeper than any framework object, before the stack runs out. */
        Object value() throws IOException, ReflectiveOperationException {
            if (++depth > MAX_DEPTH) throw new IOException("the record nests deeper than " + MAX_DEPTH + " values");
            try {
                return valueHere();
            } finally {
                depth--;
            }
        }

        private Object valueHere() throws IOException, ReflectiveOperationException {
            byte tag = data.readByte();
            switch (tag) {
                case NULL: return null;
                case STRING: return string();
                case BOOLEAN: return data.readBoolean();
                case BYTE: return data.readByte();
                case CHAR: return data.readChar();
                case SHORT: return data.readShort();
                case INT: return data.readInt();
                case LONG: return data.readLong();
                case FLOAT: return data.readFloat();
                case DOUBLE: return data.readDouble();
                case UUID: return java.util.UUID.fromString(string());
                case REFERENCE: {
                    int index = data.readInt();
                    if (index < 0 || index >= seen.size()) throw new IOException("bad reference in record");
                    return seen.get(index);
                }
                case ARRAY: {
                    Class<?> component = type(string());
                    int length = data.readInt();
                    if (length < 0 || length > data.available()) throw new IOException("truncated record");
                    Object array = Array.newInstance(component, length);
                    seen.add(array);
                    for (int i = 0; i < length; i++) Array.set(array, i, value());
                    return array;
                }
                case BUNDLE: {
                    Bundle bundle = new Bundle();
                    seen.add(bundle);
                    for (int i = data.readInt(); i > 0; i--) {
                        String key = string();
                        put(bundle, key, value());
                    }
                    return bundle;
                }
                case LIST: {
                    List<Object> list = new ArrayList<>();
                    seen.add(list);
                    for (int i = data.readInt(); i > 0; i--) list.add(value());
                    return list;
                }
                case SET: {
                    android.util.ArraySet<Object> set = new android.util.ArraySet<>();
                    seen.add(set);
                    for (int i = data.readInt(); i > 0; i--) set.add(value());
                    return set;
                }
                case OBJECT: {
                    // Only a framework class is ever looked up, and none is initialized by the lookup.
                    String name = string();
                    if (!name.startsWith("android.")) throw new IOException("unexpected class in record: " + name);
                    Class<?> type = Class.forName(name, false, Record.class.getClassLoader());
                    Object object = newInstance(type);
                    seen.add(object);
                    for (Field field : fields(type)) field.set(object, value());
                    return object;
                }
                default: throw new IOException("bad value in record: " + tag);
            }
        }
    }

    private static Class<?> type(String name) throws ClassNotFoundException {
        switch (name) {
            case "boolean": return boolean.class;
            case "byte": return byte.class;
            case "char": return char.class;
            case "short": return short.class;
            case "int": return int.class;
            case "long": return long.class;
            case "float": return float.class;
            case "double": return double.class;
            case "java.lang.String": return String.class;
            default:
                // A record names only the framework's own classes, and arrays of them, of strings or of
                // primitives; nothing else is loaded from one.
                String element = name.replaceFirst("^\\[+", "");
                boolean allowed = name.startsWith("[")
                        ? element.matches("[ZBCSIJFD]|Ljava\\.lang\\.String;|Landroid\\.[\\w.$]+;")
                        : name.startsWith("android.");
                if (!allowed) throw new ClassNotFoundException("unexpected class in record: " + name);
                return Class.forName(name, false, Record.class.getClassLoader());
        }
    }

    private static Object newInstance(Class<?> type) throws ReflectiveOperationException {
        try {
            java.lang.reflect.Constructor<?> constructor = type.getDeclaredConstructor();
            constructor.setAccessible(true);
            return constructor.newInstance();
        } catch (NoSuchMethodException e) {
            // Every field is set from the record next, so no constructor needs to run.
            Field unsafe = Class.forName("sun.misc.Unsafe").getDeclaredField("theUnsafe");
            unsafe.setAccessible(true);
            Object instance = unsafe.get(null);
            return instance.getClass().getMethod("allocateInstance", Class.class).invoke(instance, type);
        }
    }

    /** Bundle values a manifest's meta-data can hold, put back with their own types. */
    private static void put(Bundle bundle, String key, Object value) throws IOException {
        if (value == null) bundle.putString(key, null);
        else if (value instanceof String) bundle.putString(key, (String) value);
        else if (value instanceof Integer) bundle.putInt(key, (Integer) value);
        else if (value instanceof Boolean) bundle.putBoolean(key, (Boolean) value);
        else if (value instanceof Float) bundle.putFloat(key, (Float) value);
        else if (value instanceof Long) bundle.putLong(key, (Long) value);
        else if (value instanceof Double) bundle.putDouble(key, (Double) value);
        else throw new IOException("unexpected meta-data value " + value.getClass().getName());
    }
}
