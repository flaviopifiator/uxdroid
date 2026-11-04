"""Tests gles_jni.py's classification. Run: python3 -m unittest test_gles_jni (in crates/system)."""

import unittest

from gles_jni import c_arities, check_arities, classify, count, emit, needed

JAVAP = [
    ("android.opengl.GLES20", "glDrawElements", "(IIII)V"),
    ("android.opengl.GLES20", "glDrawElements", "(IIILjava/nio/Buffer;)V"),
    ("android.opengl.GLES20", "glGenTextures", "(I[II)V"),
    ("android.opengl.GLES20", "glUniformMatrix4fv", "(IIZLjava/nio/FloatBuffer;)V"),
    ("android.opengl.GLES20", "glGetShaderSource", "(IILjava/nio/IntBuffer;B)V"),
    ("android.opengl.GLES31", "glProgramUniform4fv", "(III[FI)V"),
    ("android.opengl.GLES31", "glProgramUniformMatrix3fv", "(IIIZLjava/nio/FloatBuffer;)V"),
    ("android.opengl.GLES31", "glGetProgramResourceiv", "(IIII[III[II[II)V"),
    ("android.opengl.GLES31", "glGetProgramResourceiv",
     "(IIIILjava/nio/IntBuffer;ILjava/nio/IntBuffer;Ljava/nio/IntBuffer;)V"),
    ("android.opengl.GLES31", "glCreateShaderProgramv", "(I[Ljava/lang/String;)I"),
    ("android.opengl.GLES31", "glGetProgramResourceName", "(III)Ljava/lang/String;"),
    ("android.opengl.GLES32", "glGetnUniformfv", "(III[FI)V"),
    ("android.opengl.GLES32", "glTexParameterIiv", "(II[II)V"),
    ("android.opengl.GLES32", "glDebugMessageControl", "(IIII[IIZ)V"),
    ("android.opengl.GLES32", "glReadnPixels", "(IIIIIIILjava/nio/Buffer;)V"),
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

    def test_a_program_uniform_holds_its_count_of_vectors_or_matrices(self):
        kinds = self.entries()[("glProgramUniform4fv", "(III[FI)V")]
        self.assertEqual(needed("glProgramUniform4fv", kinds, 3), "usize::saturating_mul(a2 as usize, 4)")
        kinds = self.entries()[("glProgramUniformMatrix3fv", "(IIIZLjava/nio/FloatBuffer;)V")]
        self.assertEqual(needed("glProgramUniformMatrix3fv", kinds, 4), "usize::saturating_mul(a2 as usize, 9)")

    def test_a_program_resource_query_holds_its_properties_one_length_and_its_buffer_size(self):
        kinds = self.entries()[("glGetProgramResourceiv", "(IIII[III[II[II)V")]
        self.assertEqual([needed("glGetProgramResourceiv", kinds, i) for i in (4, 7, 9)],
                         ["a3 as usize", "1", "a6 as usize"])
        kinds = self.entries()[("glGetProgramResourceiv",
                                "(IIIILjava/nio/IntBuffer;ILjava/nio/IntBuffer;Ljava/nio/IntBuffer;)V")]
        self.assertEqual([needed("glGetProgramResourceiv", kinds, i) for i in (4, 6, 7)],
                         ["a3 as usize", "1", "a5 as usize"])

    def test_a_robust_query_counts_its_buffer_in_bytes(self):
        kinds = self.entries()[("glGetnUniformfv", "(III[FI)V")]
        self.assertEqual(count(needed("glGetnUniformfv", kinds, 3)), "(a2.max(0) as usize).div_ceil(4)")

    def test_a_robust_read_holds_the_size_it_gives(self):
        kinds = self.entries()[("glReadnPixels", "(IIIIIIILjava/nio/Buffer;)V")]
        self.assertEqual(needed("glReadnPixels", kinds, 7), "a6 as usize")

    def test_a_border_color_is_four_values(self):
        kinds = self.entries()[("glTexParameterIiv", "(II[II)V")]
        self.assertEqual(needed("glTexParameterIiv", kinds, 2),
                         "if a1 as u32 == GL_TEXTURE_BORDER_COLOR { 4 } else { 1 }")

    def test_debug_message_ids_follow_their_count(self):
        kinds = self.entries()[("glDebugMessageControl", "(IIII[IIZ)V")]
        self.assertEqual(needed("glDebugMessageControl", kinds, 4), "a3 as usize")

    def test_shapes_nobody_binds_throw_by_name(self):
        entries = classify(JAVAP)
        found = {(name, descriptor): kinds for _, name, descriptor, kinds, _ in entries}
        self.assertIsNone(found[("glCreateShaderProgramv", "(I[Ljava/lang/String;)I")])
        self.assertIsNone(found[("glGetProgramResourceName", "(III)Ljava/lang/String;")])
        code = emit([e for e in entries if e[1] == "glGetProgramResourceName"])
        self.assertIn('unsupported(env, c"glGetProgramResourceName(III)Ljava/lang/String;")', code)
        self.assertIn("-> jobject", code)

    def test_a_call_with_other_arity_than_the_c_function_is_refused(self):
        table = 'c"glGenTextures", "void(i32,ptr)" c"glDrawElements", "void(u32,i32,u32,ptr)"'
        good = [e for e in classify(JAVAP) if e[1] in ("glGenTextures", "glDrawElements")]
        check_arities(good, c_arities(table))
        with self.assertRaises(SystemExit):
            check_arities(good, c_arities('c"glGenTextures", "void(i32,i32,ptr)" c"glDrawElements", "void(u32,i32,u32,ptr)"'))


if __name__ == "__main__":
    unittest.main()
