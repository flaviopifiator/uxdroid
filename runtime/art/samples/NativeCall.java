/** Exercises the JNI boundary between DEX and a bionic-linked library. */
public class NativeCall {
    static native int add(int left, int right);
    static native String greet(String name);
    static native int callBack();
    static native void throwFromNative();

    static int fromNative(int value) {
        return value * 2;
    }

    public static void main(String[] args) {
        System.loadLibrary("nativecall");
        System.out.println("add: " + add(2, 3));
        System.out.println("greet: " + greet("dex"));
        System.out.println("callBack: " + callBack());
        try {
            throwFromNative();
            System.out.println("no exception");
        } catch (IllegalStateException error) {
            System.out.println("caught: " + error.getMessage());
        }
    }
}
