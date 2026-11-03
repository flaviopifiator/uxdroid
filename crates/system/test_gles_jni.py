"""Tests gles_jni.py's classification. Run: python3 -m unittest test_gles_jni (in crates/system)."""

import unittest

from gles_jni import c_arities, check_arities, classify, count, needed

JAVAP = [
    ("android.opengl.GLES20", "glDrawElements", "(IIII)V"),
    ("android.opengl.GLES20", "glDrawElements", "(IIILjava/nio/Buffer;)V"),
    ("android.opengl.GLES20", "glGenTextures", "(I[II)V"),
    ("android.opengl.GLES20", "glUniformMatrix4fv", "(IIZLjava/nio/FloatBuffer;)V"),
    ("android.opengl.GLES20", "glGetShaderSource", "(IILjava/nio/IntBuffer;B)V"),
]


class ClassifyTest(unittest.TestCase):
    def entries(self):
        return {(name, descriptor): kinds for _, name, descriptor, kinds, _ in classify(JAVAP)}

    def test_an_int_where_an_overload_takes_a_buffer_is_an_offset(self):
        self.assertEqual(self.entries()[("glDrawElements", "(IIII)V")][3], ("offset",))

    def test_an_array_takes_the_offset_after_it(self):
        self.assertEqual(self.entries()[("glGenTextures", "(I[II)V")], [("value", "I"), ("array", "[I"), ("skip",)])

    def test_a_byte_where_a_buffer_is_meant_is_not_bound(self):
        self.assertIsNone(self.entries()[("glGetShaderSource", "(IILjava/nio/IntBuffer;B)V")])

    def test_what_an_output_must_hold_follows_its_count(self):
        kinds = self.entries()[("glGenTextures", "(I[II)V")]
        self.assertEqual(count(needed("glGenTextures", kinds, 1)), "a0.max(0) as usize")
        kinds = self.entries()[("glUniformMatrix4fv", "(IIZLjava/nio/FloatBuffer;)V")]
        self.assertEqual(needed("glUniformMatrix4fv", kinds, 3), "usize::saturating_mul(a1 as usize, 16)")

    def test_a_call_with_other_arity_than_the_c_function_is_refused(self):
        table = 'c"glGenTextures", "void(i32,ptr)" c"glDrawElements", "void(u32,i32,u32,ptr)"'
        good = [e for e in classify(JAVAP) if e[1] in ("glGenTextures", "glDrawElements")]
        check_arities(good, c_arities(table))
        with self.assertRaises(SystemExit):
            check_arities(good, c_arities('c"glGenTextures", "void(i32,i32,ptr)" c"glDrawElements", "void(u32,i32,u32,ptr)"'))


if __name__ == "__main__":
    unittest.main()
