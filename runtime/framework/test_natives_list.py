"""Tests natives_list.py. Run: python3 -m unittest test_natives_list"""

import unittest

from natives_list import natives

JAVAP = """Compiled from "Trace.java"
public final class android.os.Trace {
  public static final long TRACE_TAG_VIEW;
    descriptor: J
  private static native boolean nativeIsTagEnabled(long);
    descriptor: (J)Z
  public static void beginSection(java.lang.String);
    descriptor: (Ljava/lang/String;)V
}
Compiled from "Foo.java"
public class android.os.Foo$Bar<T> extends java.lang.Object {
  static native java.lang.String[] names(int, T) throws java.io.IOException;
    descriptor: (ILjava/lang/Object;)[Ljava/lang/String;
}
Compiled from "ColorSpace.java"
class android.graphics.ColorSpace$Rgb$Native {
  static native long nativeCreate(float, float);
    descriptor: (FF)J
}
Compiled from "Integer.java"
public final class java.lang.Integer {
  public static native int bitCount(int);
    descriptor: (I)I
}
""".splitlines()


class NativesTest(unittest.TestCase):
    def test_lists_natives_with_descriptors_in_jni_form(self):
        self.assertEqual(natives(JAVAP, {"java.lang.Integer"}), [
            ("android/os/Trace", "nativeIsTagEnabled", "(J)Z"),
            ("android/os/Foo$Bar", "names", "(ILjava/lang/Object;)[Ljava/lang/String;"),
            ("android/graphics/ColorSpace$Rgb$Native", "nativeCreate", "(FF)J"),
        ])

    def test_keeps_boot_classes_out(self):
        self.assertNotIn("java/lang/Integer", [c for c, _, _ in natives(JAVAP, {"java.lang.Integer"})])
        self.assertIn("java/lang/Integer", [c for c, _, _ in natives(JAVAP, set())])


if __name__ == "__main__":
    unittest.main()
