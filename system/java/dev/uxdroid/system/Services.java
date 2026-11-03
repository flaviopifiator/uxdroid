package dev.uxdroid.system;

import android.os.IBinder;
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.util.Map;
import java.util.TreeMap;
import java.util.concurrent.ConcurrentHashMap;

/**
 * System services inside the app's process, with no IPC. Each one is a proxy of its AIDL interface:
 * a method its handler implements answers, and any other returns its type's default value. The first
 * call of each such method is reported, so running apps shows which calls matter.
 */
public final class Services {
    private Services() {}

    private static final Map<String, Class<?>> INTERFACES = new ConcurrentHashMap<>();
    private static final Map<String, Object> HANDLERS = new ConcurrentHashMap<>();
    private static final Map<String, Integer> UNHANDLED = new TreeMap<>();
    private static IBinder serviceManager;

    static {
        INTERFACES.put("window", android.view.IWindowManager.class);
    }

    /**
     * Serves {@code name} through {@code type}. Public methods of {@code handler} with the same name
     * and parameters as an interface method answer it.
     */
    public static void register(String name, Class<?> type, Object handler) {
        INTERFACES.put(name, type);
        if (handler != null) {
            HANDLERS.put(name, handler);
        } else {
            HANDLERS.remove(name);
        }
    }

    /** Calls nobody implements yet, with how many times each was made. */
    public static synchronized Map<String, Integer> unhandled() {
        return new TreeMap<>(UNHANDLED);
    }

    /** What BinderInternal.getContextObject() returns: the service manager. */
    static synchronized IBinder contextObject() {
        if (serviceManager == null) {
            serviceManager = binder(android.os.IServiceManager.class, new ServiceManager());
        }
        return serviceManager;
    }

    /** The handler of IServiceManager. Android 16 looks services up through getService2. */
    public static final class ServiceManager {
        public IBinder getService(String name) { return service(name); }
        public IBinder checkService(String name) { return service(name); }
        public android.os.Service getService2(String name) { return wrap(service(name)); }
        public android.os.Service checkService2(String name) { return wrap(service(name)); }
    }

    private static android.os.Service wrap(IBinder binder) {
        android.os.ServiceWithMetadata found = new android.os.ServiceWithMetadata();
        found.service = binder;
        return android.os.Service.serviceWithMetadata(found);
    }

    private static IBinder service(String name) {
        Class<?> type = INTERFACES.get(name);
        if (type == null) {
            count("service " + name);
            return null;
        }
        return binder(type, HANDLERS.get(name));
    }

    private static synchronized void count(String call) {
        Integer before = UNHANDLED.put(call, UNHANDLED.getOrDefault(call, 0) + 1);
        if (before == null) {
            System.err.println("uxdroid: unhandled " + call + " returned its default");
        }
    }

    /** The service itself, for callers that hold an interface rather than a binder. */
    static <T> T local(Class<T> type, Object handler) {
        return type.cast(binder(type, handler).queryLocalInterface(type.getName()));
    }

    /** A local binder: queryLocalInterface hands back the service itself, so no Parcel is ever made. */
    static IBinder binder(Class<?> type, Object handler) {
        // The service and its binder answer for each other, so asBinder always gives the same object.
        IBinder[] self = new IBinder[1];
        Object service = Proxy.newProxyInstance(type.getClassLoader(), new Class<?>[] {type},
                dispatch(type, handler, self));
        String descriptor = type.getName();
        self[0] = (IBinder) Proxy.newProxyInstance(IBinder.class.getClassLoader(), new Class<?>[] {IBinder.class},
                (proxy, method, args) -> {
                    switch (method.getName()) {
                        case "queryLocalInterface": return service;
                        case "getInterfaceDescriptor": return descriptor;
                        case "isBinderAlive": case "pingBinder": return true;
                        case "hashCode": return System.identityHashCode(proxy);
                        case "equals": return proxy == args[0];
                        case "toString": return "Binder(" + descriptor + ")";
                        default: return defaultValue(method.getReturnType());
                    }
                });
        return self[0];
    }

    private static InvocationHandler dispatch(Class<?> type, Object handler, IBinder[] binder) {
        return (proxy, method, args) -> {
            switch (method.getName()) {
                case "hashCode": if (args == null) return System.identityHashCode(proxy); break;
                case "equals": if (args != null && args.length == 1) return proxy == args[0]; break;
                case "toString": if (args == null) return type.getSimpleName() + " in process"; break;
                case "asBinder": if (args == null) return binder[0]; break;
                default: break;
            }
            if (handler != null) {
                try {
                    Method own = handler.getClass().getMethod(method.getName(), method.getParameterTypes());
                    return own.invoke(handler, args);
                } catch (NoSuchMethodException notImplemented) {
                    // Falls through to the default.
                } catch (java.lang.reflect.InvocationTargetException failed) {
                    throw failed.getCause();
                }
            }
            count(type.getSimpleName() + "." + method.getName());
            return defaultValue(method.getReturnType());
        };
    }

    static Object defaultValue(Class<?> type) {
        if (!type.isPrimitive() || type == void.class) return null;
        if (type == boolean.class) return false;
        if (type == char.class) return (char) 0;
        if (type == byte.class) return (byte) 0;
        if (type == short.class) return (short) 0;
        if (type == int.class) return 0;
        if (type == long.class) return 0L;
        if (type == float.class) return 0f;
        return 0d;
    }
}
