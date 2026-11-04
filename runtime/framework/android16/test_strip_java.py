"""Tests strip_java.py. Run: python3 -m unittest test_strip_java"""

import unittest

from strip_java import strip

SOURCE = """package {
    default_visibility: ["//visibility:private"],
}

// A comment with a { brace.
java_library {
    name: "framework",
    srcs: ["a/{b}.java"], // } in a comment
}

cc_library {
    name: "libandroid_runtime",
    cflags: ["-DX=\\"}\\""],
    target: { host: { srcs: ["x.cpp"] } },
}

flags = ["-Wall"]

droidstubs {
    name: "api-stubs",
}

filegroup {
    name: "aidl",
}
build = [
    "AconfigFlags.bp",
]
"""


class StripTest(unittest.TestCase):
    def test_java_and_stub_modules_go_and_everything_else_stays(self):
        out = strip(SOURCE)
        self.assertNotIn('name: "framework"', out)
        self.assertNotIn("api-stubs", out)
        for kept in ("package {", 'name: "libandroid_runtime"', 'cflags: ["-DX=\\"}\\""]',
                     'srcs: ["x.cpp"]', 'flags = ["-Wall"]', 'name: "aidl"', '"AconfigFlags.bp"',
                     "// A comment with a { brace."):
            self.assertIn(kept, out)

    def test_a_file_without_java_is_unchanged(self):
        text = 'cc_binary {\n    name: "a",\n}\n\nfilegroup {\n    name: "b",\n}\n'
        self.assertEqual(strip(text), text)


if __name__ == "__main__":
    unittest.main()
