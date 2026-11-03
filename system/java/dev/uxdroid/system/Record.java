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
    private static final String MAGIC = "uxdroid package record 1";
    /** Files of the runtime a record depends on: the classes it holds and the natives its library lists. */
    private static final String[] RUNTIME_FILES = {
        "framework/framework.jar", "framework/uxdroid-system.jar", "framework/natives.txt",
    };

    /** What a start needs from the parsed manifest. */
    static final class Contents {
        final android.content.pm.ApplicationInfo application;
        final android.content.pm.ActivityInfo activity;
        /** PackageManager.getPackageInfo with flags 0, decoded again for each caller. */
        final byte[] packageInfo;

        Contents(android.content.pm.ApplicationInfo application, android.content.pm.ActivityInfo activity,
                byte[] packageInfo) {
            this.application = application;
            this.activity = activity;
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

    /** The runtime and the APK, by path, size and modification time. Changing either makes a record stale. */
    static String identity(String root, File apk) {
        StringBuilder text = new StringBuilder(new File(root).getAbsolutePath());
        for (String name : RUNTIME_FILES) {
            File file = new File(root, name);
            text.append('|').append(name).append(' ').append(file.length()).append(' ').append(file.lastModified());
        }
        return text.append("|apk ").append(apk.length()).append(' ').append(apk.lastModified()).toString();
    }

    static void write(File file, String identity, android.content.pm.ApplicationInfo application,
            android.content.pm.ActivityInfo activity, android.content.pm.PackageInfo packageInfo)
            throws IOException, ReflectiveOperationException {
        Writer info = new Writer();
        info.value(packageInfo);
        Writer out = new Writer();
        out.data.writeUTF(MAGIC);
        out.data.writeUTF(identity);
        out.value(application);
        out.value(activity);
        out.bytes(info.toByteArray());
        java.nio.file.Files.write(file.toPath(), out.toByteArray());
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
        Reader in = new Reader(java.nio.file.Files.readAllBytes(file.toPath()));
        if (!MAGIC.equals(in.data.readUTF())) throw new IOException("not a record of this version");
        if (!identity.equals(in.data.readUTF())) throw new IOException("the runtime or the APK changed since install");
        android.content.pm.ApplicationInfo application = (android.content.pm.ApplicationInfo) in.value();
        android.content.pm.ActivityInfo activity = (android.content.pm.ActivityInfo) in.value();
        return new Contents(application, activity, in.bytes());
    }

    private static final byte NULL = 0, REFERENCE = 1, OBJECT = 2, STRING = 3, ARRAY = 4, BUNDLE = 5, LIST = 6,
            BOOLEAN = 7, BYTE = 8, CHAR = 9, SHORT = 10, INT = 11, LONG = 12, FLOAT = 13, DOUBLE = 14, UUID = 15;

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
                    if (!component.isPrimitive() && component != String.class
                            && !component.getName().startsWith("android.")) {
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

    private static final class Reader {
        final DataInputStream data;
        final List<Object> seen = new ArrayList<>();

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

        Object value() throws IOException, ReflectiveOperationException {
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
                case OBJECT: {
                    Class<?> type = Class.forName(string());
                    if (!type.getName().startsWith("android.")) throw new IOException("unexpected class in record");
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
                // A record names only the framework's own classes; nothing else is loaded from one.
                if (!name.startsWith("android.")) throw new ClassNotFoundException("unexpected class in record: " + name);
                return Class.forName(name);
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
