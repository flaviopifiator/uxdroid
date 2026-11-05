#!/usr/bin/env python3
"""Writes libc_table.rs: a guest signature for every function the NDK's arm64 libc.so, libm.so and
libdl.so export whose arguments mean the same to arm64 bionic and x86_64 bionic, and the reason
each other export is left out. Run in the dev image, which has the NDK:

    python3 crates/bridge/src/guest/libc_table.py > crates/bridge/src/guest/libc_table.rs

Why x86_64 bionic: the host side of a guest call is the function x86_64 bionic code binds to, a shim
or a glibc function audited against x86_64 bionic (shims.rs, glibc_compatible.txt). Both bionics
are LP64, so where every struct a function points at has the same layout on both, that audit
covers the guest too. Layouts come from clang's record layout dump for each target; prototypes
from the headers' AST.
"""
import json
import os
import re
import subprocess
import sys
import threading

NDK = os.environ["ANDROID_NDK_ROOT"]
PREBUILT = f"{NDK}/toolchains/llvm/prebuilt/linux-x86_64"
SYSROOT = f"{PREBUILT}/sysroot"
API = 37
GUEST = f"aarch64-linux-android{API}"
HOST = f"x86_64-linux-android{API}"
HEADER_DIRS = ["", "sys/", "arpa/", "netinet/", "net/", "android/"]
# Headers that only make sense in C++ or pull in kernel internals.
SKIP_HEADERS = {"sys/user.h", "sys/ucontext.h", "sys/reg.h", "sys/procfs.h", "threads.h"}

# Calls written by hand in libc.rs, or deliberately left to the host's guards: their arguments
# carry constants or state with arm64 meanings the generic path would get wrong.
BY_HAND = {
    **{name: "open flags differ on arm64 (O_DIRECTORY, O_NOFOLLOW, O_DIRECT, O_LARGEFILE)" for name in [
        "open", "open64", "openat", "openat64", "__open_2", "__openat_2", "fcntl", "fcntl64",
        "pipe2", "mkostemp", "mkostemp64", "mkostemps", "mkostemps64"]},
    **{name: "jmp_buf holds guest registers" for name in [
        "setjmp", "_setjmp", "sigsetjmp", "longjmp", "_longjmp", "siglongjmp"]},
    **{name: "floating-point environment is the guest CPU's" for name in [
        "feclearexcept", "fegetexceptflag", "feraiseexcept", "fesetexceptflag", "fetestexcept",
        "fegetround", "fesetround", "fegetenv", "feholdexcept", "fesetenv", "feupdateenv",
        "feenableexcept", "fedisableexcept", "fegetexcept"]},
    **{name: "returns addresses guest code calls" for name in [
        "dlopen", "dlsym", "dlvsym", "dlerror", "dladdr", "dlclose", "dl_iterate_phdr",
        "android_dlopen_ext", "dl_unwind_find_exidx"]},
    "ioctl": "request numbers point at structs of any layout",
    "syscall": "system call numbers differ on arm64",
    "getauxval": "reports the guest CPU",
    "ptrace": "register sets differ on arm64",
    "vfork": "the child would share the guest stack",
    "__cxa_thread_atexit_impl": "takes a guest callback",
    "tcgetattr": "bionic's termios is the kernel's, smaller than glibc's",
    "tcsetattr": "bionic's termios is the kernel's, smaller than glibc's",
    "cfmakeraw": "bionic's termios is the kernel's, smaller than glibc's",
}

# Types whose dumps differ in spelling only: the same bits at the same offsets on both targets.
EQUIVALENT = {
    "sigset_t": "one 64-bit word of signal bits; a struct of one unsigned long on arm64",
    "sigset64_t": "one 64-bit word of signal bits; a struct of one unsigned long on arm64",
}

SCALARS = {
    "void": "void", "_Bool": "u8", "bool": "u8", "char": "i8", "signed char": "i8", "unsigned char": "u8",
    "short": "i16", "unsigned short": "u16", "int": "i32", "unsigned int": "u32",
    "long": "i64", "unsigned long": "u64", "long long": "i64", "unsigned long long": "u64",
    "float": "f32", "double": "f64", "__builtin_va_list": "va_list", "va_list": "va_list",
    "__int128": None, "unsigned __int128": None, "long double": None,
}
QUALIFIERS = re.compile(r"\b(const|volatile|restrict|__restrict|_Nonnull|_Nullable|_Null_unspecified)\b")


def parallel_map(function, items):
    """`map` over threads: each item is a clang run. The image's Python has no concurrent.futures."""
    items = list(items)
    results = [None] * len(items)
    next_index = [0]
    lock = threading.Lock()

    def work():
        while True:
            with lock:
                index = next_index[0]
                next_index[0] += 1
            if index >= len(items):
                return
            results[index] = function(items[index])

    workers = [threading.Thread(target=work) for _ in range(os.cpu_count() or 4)]
    for worker in workers:
        worker.start()
    for worker in workers:
        worker.join()
    return results


def run(arguments, **options):
    return subprocess.run(arguments, capture_output=True, text=True, **options)


def exports():
    functions = {}
    for library in ("libc", "libm", "libdl"):
        path = f"{SYSROOT}/usr/lib/aarch64-linux-android/{API}/{library}.so"
        listing = run([f"{PREBUILT}/bin/llvm-nm", "-D", "--defined-only", path], check=True).stdout
        for line in listing.splitlines():
            fields = line.split()
            if len(fields) >= 2 and fields[-2] in ("T", "W", "i"):
                functions[fields[-1].split("@")[0]] = library
    return functions


def clang(target, source, *extra):
    return run([f"{PREBUILT}/bin/clang", f"--target={target}", "-D_GNU_SOURCE", "-fsyntax-only",
                "-w", *extra, "-x", "c", "-"], input=source)


FORTIFY = ["-D_FORTIFY_SOURCE=2", "-O2"]


def compiles(target, source):
    return clang(target, source).returncode == 0


def headers():
    root = f"{SYSROOT}/usr/include/"
    found = []
    for directory in HEADER_DIRS:
        for name in sorted(os.listdir(root + directory)):
            header = directory + name
            if name.endswith(".h") and header not in SKIP_HEADERS:
                found.append(header)
    usable = parallel_map(lambda h: compiles(GUEST, f"#include <{h}>\n"), found)
    return [header for header, ok in zip(found, usable) if ok]


def prelude(included):
    return "".join(f"#include <{header}>\n" for header in included)


def declarations(included, *extra):
    result = clang(GUEST, prelude(included), *extra, "-Xclang", "-ast-dump=json")
    result.check_returncode()
    dump = result.stdout
    found = {}
    typedefs = {}
    for node in json.loads(dump).get("inner", []):
        if node.get("kind") == "TypedefDecl" and "name" in node:
            kind = node["type"]
            typedefs[node["name"]] = kind.get("desugaredQualType", kind["qualType"])
        if node.get("kind") != "FunctionDecl" or "name" not in node:
            continue
        # A renamed declaration, such as _GNU_SOURCE's strerror_r, is another symbol.
        symbol = node.get("mangledName", node["name"])
        parameters = []
        for child in node.get("inner", []):
            if child.get("kind") == "ParmVarDecl":
                kind = child["type"]
                parameters.append((kind["qualType"], kind.get("desugaredQualType", kind["qualType"])))
        function_type = node["type"]["qualType"]
        if symbol not in found or (parameters and not found[symbol][1]):
            found[symbol] = (function_type, parameters)
    return found, typedefs


def desugar(text, typedefs):
    """A type spelled through typedefs, such as a return type, in its underlying words."""
    for _ in range(16):
        bare = plain(text)
        if bare not in typedefs:
            return text
        text = typedefs[bare]
    return text


def plain(text):
    return " ".join(QUALIFIERS.sub(" ", text).split())


def classify(written, desugared):
    """('scalar', vocabulary), ('pointer', pointee), ('callback',) or ('refused', why)."""
    text = plain(desugared)
    if "(*" in text or "(^" in text:
        return ("callback",)
    if "*" in text or "[" in text:
        pointee = plain(written)
        pointee = re.sub(r"\[[^\]]*\]$", "", pointee).strip()
        pointee = pointee[: pointee.rfind("*")].strip() if "*" in pointee else pointee
        if "long double" in plain(desugared):
            return ("refused", "a long double")
        return ("pointer", pointee)
    if text.startswith("enum "):
        return ("scalar", "i32")
    if text in ("struct __va_list", "__va_list"):
        return ("scalar", "va_list")
    if text.startswith(("struct ", "union ")):
        return ("refused", f"{text} by value")
    if text in SCALARS:
        kind = SCALARS[text]
        return ("scalar", kind) if kind else ("refused", f"a {text}")
    return ("refused", f"type {text}")


def split_function_type(text):
    """The return type and whether it is variadic, from `int (const char *, ...)`."""
    depth = 0
    for index, character in enumerate(text):
        if character == "(":
            if depth == 0 and index > 0:
                return text[:index].strip(), text.rstrip().endswith("...)")
            depth += 1
        elif character == ")":
            depth -= 1
    return text, False


SCALAR_POINTEES = set(SCALARS) | {"wchar_t", "char16_t", "char32_t"}


def layout(target, included, pointee):
    """The layout of `pointee` as the only field of a probe struct, so records, scalar typedefs and
    every record nested by value show in one block; None for an incomplete type."""
    source = prelude(included) + f"struct uxdroid_probe {{ {pointee} value; }};\n" \
        "unsigned long uxdroid_size = sizeof(struct uxdroid_probe);\n"
    result = clang(target, source, "-Xclang", "-fdump-record-layouts")
    if result.returncode != 0:
        return None  # incomplete: an opaque handle, whose inside guest code never sees
    for block in result.stdout.split("*** Dumping AST Record Layout"):
        if re.search(r"^\s*0 \| struct uxdroid_probe$", block, re.M):
            return re.sub(r"(aarch64|x86_64)-linux-android", "TARGET", block)
    return None


def main():
    functions = exports()
    included = headers()
    declared, typedefs = declarations(included)
    # FORTIFY's checked functions (__memcpy_chk and its kind) are declared only with it on, which
    # also wraps the plain functions in inline overloads, so only the _chk names are taken.
    fortified, _ = declarations(included, *FORTIFY)
    for symbol, declaration in fortified.items():
        if re.fullmatch(r"__\w+_chk", symbol) and symbol not in declared:
            declared[symbol] = declaration

    pointees = set()
    shapes = {}
    for symbol in functions:
        if symbol not in declared:
            continue
        function_type, parameters = declared[symbol]
        returns, variadic = split_function_type(function_type)
        kinds = [classify(returns, desugar(returns, typedefs))]
        kinds += [classify(w, desugar(d, typedefs)) for w, d in parameters]
        shapes[symbol] = (kinds, variadic)
        for kind in kinds:
            if kind[0] == "pointer" and plain(kind[1]).split(" ")[-1] not in SCALAR_POINTEES \
                    and plain(kind[1]) not in SCALAR_POINTEES:
                pointees.add(plain(kind[1]))

    def compare(pointee):
        if pointee in EQUIVALENT:
            return pointee, "same"
        guest, host = layout(GUEST, included, pointee), layout(HOST, included, pointee)
        if guest is None or host is None:
            return pointee, "opaque" if guest is None and host is None else "differs"
        if "(*)" in guest:
            return pointee, "callback"
        return pointee, "same" if guest == host else "differs"

    verdicts = dict(parallel_map(compare, sorted(pointees)))

    signed, unsigned = [], []
    for symbol in sorted(functions):
        if symbol in BY_HAND:
            unsigned.append((symbol, BY_HAND[symbol]))
            continue
        if symbol not in shapes:
            unsigned.append((symbol, "no prototype in the NDK headers"))
            continue
        kinds, variadic = shapes[symbol]
        why = None
        vocabulary = []
        for kind in kinds:
            if kind[0] == "scalar":
                vocabulary.append(kind[1])
            elif kind[0] == "callback":
                why = "takes a callback"
            elif kind[0] == "refused":
                why = f"passes {kind[1]}"
            else:
                verdict = verdicts.get(plain(kind[1]), "same")
                if verdict == "differs":
                    why = f"{kind[1]} differs on arm64"
                elif verdict == "callback":
                    why = f"{kind[1]} holds a callback"
                vocabulary.append("ptr")
            if why:
                break
        if why is None and "va_list" in vocabulary:
            why = "takes a va_list"
        if why:
            unsigned.append((symbol, why))
            continue
        returns, parameters = vocabulary[0], vocabulary[1:]
        if parameters == ["void"]:
            parameters = []
        if variadic:
            parameters.append("...")
        signed.append((symbol, f"{returns}({','.join(parameters)})"))

    print("// Generated by libc_table.py from the NDK's arm64 libc.so, libm.so and libdl.so and their")
    print("// headers. Do not edit.")
    print()
    print("/// Functions whose arguments mean the same to arm64 and x86_64 bionic, by name, sorted.")
    print("pub(super) const SIGNED: &[(&str, &str)] = &[")
    for symbol, signature in signed:
        print(f'    ("{symbol}", "{signature}"),')
    print("];")
    print()
    print("/// The other exports, sorted, and why the generic path cannot carry them.")
    print("pub(super) const UNSIGNED: &[(&str, &str)] = &[")
    for symbol, why in unsigned:
        print(f'    ("{symbol}", "{why}"),')
    print("];")
    print(f"generated: {len(signed)} signed, {len(unsigned)} not", file=sys.stderr)


if __name__ == "__main__":
    main()
