"""Tests default_context.py. Run: python3 -m unittest test_default_context"""

import unittest

from default_context import generate

JAVAP = """public abstract class android.content.Context {
  public static final java.lang.String WINDOW_SERVICE;
  public abstract android.content.res.Resources$Theme getTheme();
  public abstract boolean deleteFile(java.lang.String);
  public abstract void startActivity(android.content.Intent);
  public abstract <T> T getSystemService(java.lang.Class<T>);
  public abstract java.io.FileInputStream openFileInput(java.lang.String) throws java.io.FileNotFoundException;
  public abstract int checkPermission(java.lang.String, int, int);
  public int getUserId();
}""".splitlines()


class DefaultContextTest(unittest.TestCase):
    def test_every_abstract_method_returns_its_default(self):
        source, count = generate(JAVAP)
        self.assertEqual(count, 6)
        self.assertIn("public android.content.res.Resources.Theme getTheme() { return null; }", source)
        self.assertIn("public boolean deleteFile(java.lang.String a0) { return false; }", source)
        self.assertIn("public void startActivity(android.content.Intent a0) { }", source)
        self.assertIn("public <T> T getSystemService(java.lang.Class<T> a0) { return null; }", source)
        self.assertIn("public java.io.FileInputStream openFileInput(java.lang.String a0) { return null; }", source)
        self.assertIn("public int checkPermission(java.lang.String a0, int a1, int a2) { return 0; }", source)
        self.assertNotIn("getUserId", source)

    def test_refuses_a_method_it_cannot_parse(self):
        with self.assertRaises(ValueError):
            generate(["  public abstract strange thing here(;"])


if __name__ == "__main__":
    unittest.main()
