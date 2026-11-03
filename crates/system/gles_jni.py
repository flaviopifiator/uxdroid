"""Generates the JNI natives of android.opengl.GLES20 and GLES30 over the host's GLES.

Usage: gles_jni.py <javap -s -p output of both classes> <crates/ndk/src/gles_table.rs> > src/gl/gles.rs
(runtime/framework/gles_jni.sh runs javap on the framework and this script.)

Each native becomes a Rust function that converts its Java arguments and calls the GL function of the
same name, looked up once in the host's libGLESv2. The conversions follow the shapes of the Java API,
the same ones AOSP's generated bindings have:

- int, float, boolean and long pass as they are; GLenum, GLuint, GLsizei and the like are 32 bits;
- an array is followed by an offset, and passes as a pointer to that element;
- a java.nio buffer passes as a pointer to its position, a direct one's memory or a heap one's array;
- an int where another overload of the same function takes a buffer, and the last argument of
  glVertexAttribPointer and glVertexAttribIPointer, is an offset into the bound GL buffer;
- a String is a C string.

Every array and buffer is checked to hold what the function reads or writes, by the rules in
`needed` below, so no GL call writes past the end of a Java array; a short one throws
IllegalArgumentException, as AOSP's bindings do. Natives with other shapes, such as those returning
a String, are written by hand in src/gl/mod.rs and listed in HAND_WRITTEN.
"""

import re
import sys

# Natives src/gl/mod.rs implements itself, by name and descriptor.
HAND_WRITTEN = {
    ("_nativeClassInit", "()V"),
    ("glGetString", "(I)Ljava/lang/String;"),
    ("glGetStringi", "(II)Ljava/lang/String;"),
    ("glGetShaderInfoLog", "(I)Ljava/lang/String;"),
    ("glGetProgramInfoLog", "(I)Ljava/lang/String;"),
    ("glGetShaderSource", "(I)Ljava/lang/String;"),
    ("glShaderSource", "(ILjava/lang/String;)V"),
    ("glGetActiveAttrib", "(II[II[II)Ljava/lang/String;"),
    ("glGetActiveAttrib", "(IILjava/nio/IntBuffer;Ljava/nio/IntBuffer;)Ljava/lang/String;"),
    ("glGetActiveUniform", "(II[II[II)Ljava/lang/String;"),
    ("glGetActiveUniform", "(IILjava/nio/IntBuffer;Ljava/nio/IntBuffer;)Ljava/lang/String;"),
    ("glGetTransformFeedbackVarying", "(II[II[II)Ljava/lang/String;"),
    ("glGetTransformFeedbackVarying", "(IILjava/nio/IntBuffer;Ljava/nio/IntBuffer;)Ljava/lang/String;"),
    ("glGetActiveUniformBlockName", "(II)Ljava/lang/String;"),
    ("glTransformFeedbackVaryings", "(I[Ljava/lang/String;I)V"),
    ("glGetUniformIndices", "(I[Ljava/lang/String;[II)V"),
    ("glGetUniformIndices", "(I[Ljava/lang/String;Ljava/nio/IntBuffer;)V"),
    ("glMapBufferRange", "(IIII)Ljava/nio/Buffer;"),
    ("glGetBufferPointerv", "(II)Ljava/nio/Buffer;"),
    ("glVertexAttribPointerBounds", "(IIIZILjava/nio/Buffer;I)V"),
    ("glVertexAttribIPointerBounds", "(IIIILjava/nio/Buffer;I)V"),
    ("glGetActiveUniformBlockName", "(IILjava/nio/Buffer;Ljava/nio/Buffer;)V"),
}

# The last int of these is an offset into the bound array buffer.
OFFSET_LAST = {"glVertexAttribPointer", "glVertexAttribIPointer"}

PRIMITIVE = {"I": ("jint", "i32"), "F": ("jfloat", "f32"), "Z": ("jboolean", "u8"), "J": ("jlong", "i64")}
ARRAY = {"[I": "i32", "[F": "f32", "[Z": "u8", "[J": "i64", "[B": "i8"}
ELEMENT_BYTES = {"Short": 2, "Char": 2, "Int": 4, "Float": 4, "Long": 8, "Double": 8}
BUFFER = re.compile(r"Ljava/nio/(Byte|Short|Char|Int|Long|Float|Double)?Buffer;")


def split_descriptor(descriptor):
    """The argument types of a method descriptor, and its return type."""
    args, ret = re.match(r"\((.*)\)(.*)", descriptor).groups()
    types, i = [], 0
    while i < len(args):
        j = i
        while args[j] == "[":
            j += 1
        if args[j] == "L":
            j = args.index(";", j)
        types.append(args[i:j + 1])
        i = j + 1
    return types, ret


def parse(text):
    """(class, name, descriptor) of every native, in order."""
    natives, klass = [], None
    lines = text.splitlines()
    for i, line in enumerate(lines):
        found = re.match(r"public class ((?:android\.opengl|com\.google\.android\.gles_jni)\.\w+)", line)
        if found:
            klass = found.group(1)
        found = re.search(r" native .* (\w+)\(.*\);", line)
        if found and i + 1 < len(lines):
            descriptor = lines[i + 1].split("descriptor:")[1].strip()
            natives.append((klass, found.group(1), descriptor))
    return natives


def needed(name, kinds, index):
    """A Rust expression for how many elements the pointer argument at `index` must hold.

    `kinds` are the classified arguments; `a<i>` names argument i's value in the generated code.
    """
    pointers = [i for i, kind in enumerate(kinds) if kind[0] in ("array", "buffer")]
    first = pointers.index(index)
    m = re.match(r"glUniformMatrix(\d)(?:x(\d))?fv$", name)
    if m:
        return "usize::saturating_mul(a1 as usize, {})".format(int(m.group(1)) * int(m.group(2) or m.group(1)))
    m = re.match(r"glUniform(\d)(f|i|ui)v$", name)
    if m:
        return "usize::saturating_mul(a1 as usize, {})".format(m.group(1))
    m = re.match(r"glVertexAttrib(\d)fv$", name)
    if m:
        return m.group(1)
    if name in ("glVertexAttribI4iv", "glVertexAttribI4uiv"):
        return "4"
    if re.match(r"gl(Gen|Delete)\w+$", name) or name == "glDrawBuffers":
        return "a0 as usize"
    if name in ("glInvalidateFramebuffer", "glInvalidateSubFramebuffer"):
        return "a1 as usize"
    if name.startswith("glClearBuffer"):
        return "if a0 as u32 == GL_COLOR { 4 } else { 1 }"
    if name == "glGetActiveUniformsiv":
        return "a1 as usize"
    if name == "glGetInternalformativ":
        return "a3 as usize"
    if name == "glGetSynciv":
        return "1" if first == 0 else "a2 as usize"
    if name == "glShaderBinary":
        return "a0 as usize" if first == 0 else "a{} as usize".format(len(kinds) - 1)
    if name == "glGetAttachedShaders":
        return "1" if first == 0 else "a1 as usize"
    if name == "glGetShaderPrecisionFormat":
        return "2" if first == 0 else "1"
    if name in ("glGetShaderSource", "glGetActiveUniformBlockName"):
        return "1" if first == 0 else "a{} as usize".format(1 if name == "glGetShaderSource" else 2)
    if name in ("glGetActiveAttrib", "glGetActiveUniform", "glGetTransformFeedbackVarying"):
        return "a2 as usize" if first == 3 else "1"
    if name in ("glBufferData", "glBufferSubData"):
        return "a1 as usize" if name == "glBufferData" else "a2 as usize"
    if name == "glReadPixels":
        return "pixel_bytes(a2, a3, a4 as u32, a5 as u32)"
    if name in ("glGetIntegerv", "glGetFloatv", "glGetBooleanv", "glGetInteger64v"):
        return "get_needed(a0 as u32)"
    if name in ("glGetIntegeri_v", "glGetInteger64i_v"):
        return "get_needed(a0 as u32)"
    if name == "glGetVertexAttribfv" or name == "glGetVertexAttribiv" or name.startswith("glGetVertexAttribI"):
        return "if a1 as u32 == GL_CURRENT_VERTEX_ATTRIB { 4 } else { 1 }"
    if name.startswith("glGetUniform") and name.endswith("v"):
        # The uniform's size is not known here; a mat4 takes 16.
        return "16"
    if re.match(r"gl(Tex|Sampler)Parameter\w*v$", name) or re.match(r"glGet(Tex|Sampler)Parameter\w*v$", name):
        return "1"
    if re.match(r"glGet\w+v$", name):
        return "1"
    # Pixel and data uploads: GL reads at most what the bound state and sizes say; AOSP checks no
    # more than that the buffer exists.
    return "0"


def count(expression):
    """Negative counts need nothing: GL refuses them itself with GL_INVALID_VALUE."""
    return re.sub(r"\ba(\d+) as usize", r"a\1.max(0) as usize", expression)


def classify(natives):
    by_name = {}
    for _, name, descriptor in natives:
        by_name.setdefault(name, []).append(split_descriptor(descriptor)[0])
    out = []
    for klass, name, descriptor in natives:
        if (name, descriptor) in HAND_WRITTEN:
            continue
        types, ret = split_descriptor(descriptor)
        if "B" in types:
            out.append((klass, name, descriptor, None, ret))
            continue
        kinds, i = [], 0
        while i < len(types):
            t = types[i]
            if t in PRIMITIVE:
                is_offset = (t == "I" and any(len(o) == len(types) and BUFFER.fullmatch(o[i])
                                              for o in by_name[name])) \
                    or (name in OFFSET_LAST and i == len(types) - 1)
                kinds.append(("offset",) if is_offset else ("value", t))
            elif t in ARRAY:
                kinds.append(("array", t))
                kinds.append(("skip",))
                i += 1
            elif BUFFER.fullmatch(t):
                kinds.append(("buffer", t))
            elif t == "Ljava/lang/String;":
                kinds.append(("string",))
            else:
                raise SystemExit("unhandled argument {} of {}{}".format(t, name, descriptor))
            i += 1
        out.append((klass, name, descriptor, kinds, ret))
    return out


def jni_type(t):
    if t in PRIMITIVE:
        return PRIMITIVE[t][0]
    if t in ARRAY:
        return "jarray"
    return "jobject"


def emit(entries):
    lines = [
        "// Generated by crates/system/gles_jni.py from the framework's android.opengl.GLES20 and GLES30.",
        "// Do not edit: run runtime/framework/gles_jni.sh.",
        "",
        "#![allow(clippy::too_many_arguments, non_snake_case, unused_parens, unused_variables)]",
        "",
        "use std::ffi::c_void;",
        "use std::sync::OnceLock;",
        "",
        "use jni_sys::{JNIEnv, jarray, jboolean, jclass, jfloat, jint, jlong, jobject};",
        "",
        "use super::jni::{GL_COLOR, GL_CURRENT_VERTEX_ATTRIB, Method, array, buffer, get_needed, gl, pixel_bytes, string, unsupported};",
        "",
    ]
    tables = {}
    for n, (klass, name, descriptor, kinds, ret) in enumerate(entries):
        types, _ = split_descriptor(descriptor)
        function = "{}_{}".format(name, n)
        tables.setdefault(klass, []).append((name, descriptor, function))
        params = "".join(", a{}: {}".format(i, jni_type(t)) for i, t in enumerate(types))
        rust_ret = {"V": "", "I": " -> jint", "Z": " -> jboolean", "J": " -> jlong"}[ret]
        default = {"V": "", "I": "0", "Z": "false", "J": "0"}[ret]
        lines.append("unsafe extern \"system\" fn {}(env: *mut JNIEnv, _class: jclass{}){} {{".format(
            function, params, rust_ret))
        if kinds is None:
            lines.append("    // SAFETY: ART passes a valid environment.")
            lines.append("    unsafe {{ unsupported(env, c\"{}{}\") }};".format(name, descriptor))
            if default:
                lines.append("    " + default)
            lines.append("}")
            lines.append("")
            continue
        c_types, c_args, prep = [], [], []
        for i, kind in enumerate(kinds):
            if kind[0] == "skip":
                continue
            if kind[0] == "value":
                c_types.append(PRIMITIVE[kind[1]][1])
                c_args.append("a{} as u8".format(i) if kind[1] == "Z" else "a{}".format(i))
            elif kind[0] == "offset":
                c_types.append("*const c_void")
                c_args.append("a{} as isize as *const c_void".format(i))
            elif kind[0] == "array":
                need = count(needed(name, kinds, i))
                size = {"[I": 4, "[F": 4, "[Z": 1, "[J": 8, "[B": 1}[kind[1]]
                prep.append("    let Some(p{i}) = (unsafe {{ array(env, a{i}, a{j}, {need}, {size}, c\"{name}\") }}) else {{ return {d}; }};"
                            .format(i=i, j=i + 1, need=need, size=size, name=name, d=default))
                c_types.append("*mut c_void")
                c_args.append("p{}.pointer()".format(i))
            elif kind[0] == "buffer":
                need = count(needed(name, kinds, i))
                nullable = "true" if need == "0" or name == "glBufferData" else "false"
                # In bytes: a typed buffer's elements are the GL type's, an untyped one counts bytes.
                size = ELEMENT_BYTES.get(BUFFER.fullmatch(kind[1]).group(1), 1)
                if size != 1 and need != "0":
                    need = "usize::saturating_mul({}, {})".format(need, size)
                prep.append("    let Some(p{i}) = (unsafe {{ buffer(env, a{i}, {need}, {nullable}, c\"{name}\") }}) else {{ return {d}; }};"
                            .format(i=i, need=need, nullable=nullable, name=name, d=default))
                c_types.append("*mut c_void")
                c_args.append("p{}.pointer()".format(i))
            elif kind[0] == "string":
                prep.append("    let Some(p{i}) = (unsafe {{ string(env, a{i}, c\"{name}\") }}) else {{ return {d}; }};"
                            .format(i=i, name=name, d=default))
                c_types.append("*const std::ffi::c_char")
                c_args.append("p{}.as_ptr()".format(i))
        c_ret = {"V": "", "I": " -> i32", "Z": " -> u8", "J": " -> i64"}[ret]
        lines.append("    static ADDRESS: OnceLock<usize> = OnceLock::new();")
        lines.append("    // SAFETY: the host's {} has this C signature.".format(name))
        lines.append("    let Some(f) = (unsafe {{ gl::<unsafe extern \"C\" fn({}){}>(env, &ADDRESS, c\"{}\") }}) else {{ return {}; }};"
                     .format(", ".join(c_types), c_ret, name, default))
        lines.extend(prep)
        lines.append("    // SAFETY: the arguments were checked against what the function reads and writes.")
        call = "unsafe {{ f({}) }}".format(", ".join(c_args))
        lines.append("    " + ("(" + call + " != 0)" if ret == "Z" else call + (";" if ret == "V" else "")))
        lines.append("}")
        lines.append("")
    for klass, methods in tables.items():
        const = klass.split(".")[-1].upper()
        lines.append("/// The generated natives of `{}`.".format(klass))
        lines.append("pub(super) const {}: &[Method] = &[".format(const))
        for name, descriptor, function in methods:
            lines.append("    (c\"{}\", c\"{}\", {} as *mut c_void),".format(name, descriptor, function))
        lines.append("];")
        lines.append("")
    return "\n".join(lines)


def c_arities(table):
    """Each GLES function's number of C arguments, from the NDK headers' signature table."""
    arities = {}
    for name, signature in re.findall(r'c"(gl\w+)",\s*"([^"]+)"', table):
        arguments = re.match(r"[^(]*\((.*)\)$", signature).group(1)
        arities[name] = 0 if not arguments else len(arguments.split(","))
    return arities


def check_arities(entries, arities):
    """Every generated call must pass the C function as many arguments as it takes."""
    for _, name, descriptor, kinds, _ in entries:
        if kinds is None:
            continue
        passed = sum(1 for kind in kinds if kind[0] != "skip")
        if name not in arities:
            sys.exit("{}: not in the GLES table".format(name))
        if arities[name] != passed:
            sys.exit("{}{} passes {} arguments, the C function takes {}: write it by hand".format(
                name, descriptor, passed, arities[name]))


def main():
    every = parse(open(sys.argv[1]).read())
    # GLImpl, the GL10 a GLSurfaceView hands its renderer, declares many of the same functions as
    # instance methods; those reuse the GLES functions, which ignore their second argument.
    gl10 = [(name, descriptor) for klass, name, descriptor in every if klass.endswith(".GLImpl")]
    natives = [native for native in every if not native[0].endswith(".GLImpl")]
    if not natives:
        sys.exit("no natives in " + sys.argv[1])
    entries = classify(natives)
    check_arities(entries, c_arities(open(sys.argv[2]).read()))
    print(emit(entries))
    print("/// The natives `com.google.android.gles_jni.GLImpl` declares, by name and descriptor.")
    print("pub(super) const GL_IMPL: &[(&std::ffi::CStr, &std::ffi::CStr)] = &[")
    for name, descriptor in gl10:
        print("    (c\"{}\", c\"{}\"),".format(name, descriptor))
    print("];")
    print("generated {} natives, {} by hand".format(len(entries), len(HAND_WRITTEN)), file=sys.stderr)


if __name__ == "__main__":
    main()
