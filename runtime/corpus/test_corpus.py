"""Tests for the catalog runner's pure logic: index selection, the flat-screenshot check, the choice of
a tap target, UI comparison, and blocker extraction, normalization and ranking.

Run in the dev container: docker compose run --rm dev python3 runtime/corpus/test_corpus.py
"""

import json
import os
import struct
import subprocess
import sys
import traceback
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")
sys.dont_write_bytecode = True  # no __pycache__ in the source tree
sys.path.insert(0, HERE)

import corpus  # noqa: E402


class Check:
    """The few `unittest` assertions these tests use: the dev image's Python is python3-minimal,
    which has no `unittest`."""

    def setUp(self):
        pass

    def assertEqual(self, a, b):
        assert a == b, f"{a!r} != {b!r}"

    def assertNotEqual(self, a, b):
        assert a != b, f"{a!r} == {b!r}"

    def assertTrue(self, value):
        assert value, f"{value!r} is not true"

    def assertFalse(self, value):
        assert not value, f"{value!r} is not false"

    def assertIn(self, item, container):
        assert item in container, f"{item!r} not in {container!r}"

    def assertNotIn(self, item, container):
        assert item not in container, f"{item!r} in {container!r}"

    def assertIsNone(self, value):
        assert value is None, f"{value!r} is not None"

    def assertRaises(self, kind):
        class Raises:
            def __enter__(self):
                return self

            def __exit__(self, type_, value, tb):
                assert type_ is not None and issubclass(type_, kind), f"{kind.__name__} not raised"
                return True

        return Raises()


def main():
    failed = run = 0
    for case in [c for c in globals().values() if isinstance(c, type) and issubclass(c, Check) and c is not Check]:
        for name in sorted(n for n in dir(case) if n.startswith("test_")):
            run += 1
            test = case()
            try:
                test.setUp()
                getattr(test, name)()
            except Exception:
                failed += 1
                print(f"FAIL {case.__name__}.{name}")
                traceback.print_exc()
    print(f"{run} tests, {failed} failed")
    sys.exit(1 if failed else 0)


def fixture(name):
    with open(os.path.join(FIXTURES, name), encoding="utf-8") as f:
        return f.read()


def fixture_json(name):
    return json.loads(fixture(name))


# An independent PNG encoder for the tests: it applies the five filters forward, so the decoder
# under test has to undo each of them.


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def filtered(kind, row, prior, bpp):
    out = bytearray()
    for i, x in enumerate(row):
        a = row[i - bpp] if i >= bpp else 0
        b = prior[i]
        c = prior[i - bpp] if i >= bpp else 0
        predictor = [0, a, b, (a + b) // 2, paeth(a, b, c)][kind]
        out.append((x - predictor) & 0xFF)
    return bytes(out)


def make_png(width, height, pixel, color_type=2, filters=(0, 1, 2, 3, 4)):
    """`pixel(x, y)` gives the channel tuple of a pixel; row y uses filters[y % len(filters)]."""
    bpp = {0: 1, 2: 3, 4: 2, 6: 4}[color_type]
    raw = bytearray()
    prior = bytes(width * bpp)
    for y in range(height):
        row = bytes(v for x in range(width) for v in pixel(x, y))
        kind = filters[y % len(filters)]
        raw.append(kind)
        raw += filtered(kind, row, prior, bpp)
        prior = row

    def chunk(tag, body):
        return struct.pack(">I", len(body)) + tag + body + struct.pack(">I", zlib.crc32(tag + body))

    header = struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(raw)))
            + chunk(b"IEND", b""))


class SelectionTest(Check):
    def setUp(self):
        self.chosen = {r["package"]: r for r in corpus.select_releases(fixture_json("index.json"))}

    def test_newest_stable_version_with_a_usable_abi_or_none_is_chosen(self):
        self.assertEqual(self.chosen["org.example.plain"]["version_code"], 3)
        self.assertEqual(self.chosen["org.example.plain"]["abi"], "none")
        self.assertEqual(self.chosen["org.example.plain"]["file"], "/org.example.plain_3.apk")
        self.assertEqual(self.chosen["org.example.plain"]["sha256"], "1" * 64)
        self.assertEqual(self.chosen["org.example.plain"]["size"], 1000)
        self.assertEqual(self.chosen["org.example.plain"]["version_name"], "1.3")

    def test_x86_64_is_preferred_over_arm64(self):
        self.assertEqual(self.chosen["org.example.both"]["abi"], "x86_64")
        self.assertEqual(self.chosen["org.example.arm"]["abi"], "arm64-v8a")

    def test_newest_version_without_a_usable_abi_drops_the_app(self):
        self.assertNotIn("org.example.armonly", self.chosen)

    def test_release_channels_are_skipped(self):
        self.assertEqual(self.chosen["org.example.beta"]["version_code"], 8)

    def test_sdk_range_must_hold_36(self):
        self.assertNotIn("org.example.futuresdk", self.chosen)
        self.assertNotIn("org.example.oldsdk", self.chosen)

    def test_unsafe_or_incomplete_entries_are_dropped(self):
        self.assertNotIn("org.example.escape", self.chosen)
        self.assertNotIn("org.example.nohash", self.chosen)
        self.assertNotIn("not a/package", self.chosen)

    def test_sample_is_deterministic_and_bounded(self):
        releases = corpus.select_releases(fixture_json("index.json"))
        first = corpus.sample(releases, 2, seed=7)
        self.assertEqual(len(first), 2)
        self.assertEqual(first, corpus.sample(releases, 2, seed=7))
        self.assertEqual(len(corpus.sample(releases, 100, seed=7)), len(releases))

    def test_select_command_prints_one_tsv_row_per_app(self):
        out = subprocess.run(
            [sys.executable, "-I", os.path.join(HERE, "corpus.py"), "select",
             os.path.join(FIXTURES, "index.json")],
            check=True, capture_output=True, text=True).stdout.splitlines()
        self.assertEqual(out[0].split("\t"),
                         ["package", "version", "version_name", "abi", "file", "sha256", "size"])
        rows = [line.split("\t") for line in out[1:]]
        self.assertEqual([r[0] for r in rows], sorted(self.chosen))
        self.assertIn(["org.example.both", "10", "2.0", "x86_64", "/org.example.both_10.apk",
                       "3" * 64, "2000"], rows)


class FlatScreenshotTest(Check):
    def test_one_color_is_flat(self):
        png = make_png(40, 30, lambda x, y: (255, 255, 255))
        self.assertTrue(corpus.is_flat(png))

    def test_varied_content_through_every_filter_is_not_flat(self):
        png = make_png(40, 30, lambda x, y: ((x * 7) & 0xFF, (y * 11) & 0xFF, (x * y) & 0xFF))
        self.assertFalse(corpus.is_flat(png))

    def test_decoder_undoes_every_filter(self):
        pixel = lambda x, y: ((x * 13 + y) & 0xFF, (y * 5) & 0xFF, (x ^ y) & 0xFF, 200)  # noqa: E731
        width, height, rows = corpus.decode_png(make_png(9, 10, pixel, color_type=6))
        self.assertEqual((width, height), (9, 10))
        for y in range(10):
            self.assertEqual(rows[y], bytes(v for x in range(9) for v in pixel(x, y)))

    def test_a_small_mark_on_one_color_is_still_flat(self):
        # 1% of the pixels differ: a spinner on a blank screen.
        png = make_png(100, 100, lambda x, y: (0, 0, 0) if x < 10 and y < 10 else (30, 30, 30))
        self.assertTrue(corpus.is_flat(png))

    def test_more_than_two_percent_of_another_color_is_not_flat(self):
        png = make_png(100, 100, lambda x, y: (0, 0, 0) if y < 3 else (30, 30, 30))
        self.assertFalse(corpus.is_flat(png))

    def test_grayscale_is_decoded(self):
        self.assertTrue(corpus.is_flat(make_png(8, 8, lambda x, y: (128,), color_type=0)))

    def test_damaged_png_is_refused(self):
        with self.assertRaises(ValueError):
            corpus.decode_png(b"not a png")
        png = make_png(8, 8, lambda x, y: (1, 2, 3))
        with self.assertRaises(ValueError):
            corpus.decode_png(png[:40])


class TapTest(Check):
    def test_target_is_the_center_of_the_first_visible_clickable_leaf(self):
        self.assertEqual(corpus.tap_target(fixture_json("ui-buttons.json")), (200, 230))

    def test_windows_that_carry_their_own_bounds_are_searched_from_their_root(self):
        # The shape `uxdroid app <package> ui` prints: each window has bounds besides its root.
        self.assertEqual(corpus.tap_target(fixture_json("ui-windows-with-bounds.json")), (200, 82))

    def test_clickable_container_without_clickable_children_is_a_target(self):
        self.assertEqual(corpus.tap_target(fixture_json("ui-container-clickable.json")), (200, 150))

    def test_no_clickable_node_gives_no_target(self):
        self.assertIsNone(corpus.tap_target(fixture_json("ui-none.json")))

    def test_target_command_falls_back_to_the_center_of_the_top_window(self):
        def target(name):
            return subprocess.run(
                [sys.executable, "-I", os.path.join(HERE, "corpus.py"), "target",
                 os.path.join(FIXTURES, name)], capture_output=True, text=True)

        self.assertEqual(target("ui-buttons.json").stdout, "200 230 node\n")
        self.assertEqual(target("ui-none.json").stdout, "200 350 center\n")
        missing = target("no-such-file.json")
        self.assertEqual((missing.stdout, missing.returncode), ("", 1))

    def test_snapshot_and_node_numbers_do_not_count_as_a_change(self):
        self.assertEqual(corpus.ui_digest(fixture_json("ui-buttons.json")),
                         corpus.ui_digest(fixture_json("ui-buttons-renumbered.json")))
        self.assertNotEqual(corpus.ui_digest(fixture_json("ui-buttons.json")),
                            corpus.ui_digest(fixture_json("ui-none.json")))

    def test_tap_changed_by_the_ui_tree(self):
        before, after = fixture_json("ui-buttons.json"), fixture_json("ui-none.json")
        self.assertEqual(corpus.tap_changed(before, after, (b"a", b"a"), b"a"), "yes")
        self.assertEqual(corpus.tap_changed(before, fixture_json("ui-buttons-renumbered.json"),
                                            (b"a", b"a"), b"a"), "no")

    def test_tap_changed_by_the_screen_only_when_it_was_still_before(self):
        ui = fixture_json("ui-none.json")
        self.assertEqual(corpus.tap_changed(ui, ui, (b"a", b"a"), b"b"), "yes")
        # An animated screen changes by itself, so only the tree can show the tap's effect.
        self.assertEqual(corpus.tap_changed(ui, ui, (b"a", b"c"), b"b"), "no")


class BlockerTest(Check):
    dead = {"installed": True, "alive": False, "frame": False, "flat": None}

    def test_java_crash_reports_its_root_cause(self):
        self.assertEqual(corpus.find_blocker("", fixture("java-crash.log"), self.dead),
                         "Caused by: java.security.KeyStoreException: AndroidKeyStore not found")

    def test_java_crash_without_cause_reports_the_exception(self):
        self.assertEqual(
            corpus.find_blocker("", fixture("java-crash-no-cause.log"), self.dead),
            "java.lang.NullPointerException: Attempt to invoke virtual method "
            "'int android.view.View.getId()' on a null object reference")

    def test_native_signal_comes_before_a_caught_exception(self):
        self.assertTrue(corpus.find_blocker("", fixture("native-signal.log"), self.dead)
                        .startswith("Fatal signal 11 (SIGSEGV)"))

    def test_uxdroid_refusal_explains_the_signal_after_it(self):
        self.assertEqual(corpus.find_blocker("", fixture("refusal-then-signal.log"), self.dead),
                         "uxdroid: __pread_chk is not audited for bionic compatibility yet")

    def test_unresolved_symbols(self):
        self.assertEqual(corpus.find_blocker("", fixture("unresolved.log"), self.dead),
                         "libmegazord.so: unresolved symbols: __get_h_errno")

    def test_failed_install_reports_its_error(self):
        outcome = {"installed": False, "alive": None, "frame": False, "flat": None}
        self.assertEqual(
            corpus.find_blocker(fixture("install-failed.log"), "", outcome),
            "error: /samples/org.example.bad_3.apk: its signature does not verify: its key rotation "
            "proof uses a signature algorithm this verifier does not support")

    def test_nothing_logged_names_the_failed_check(self):
        self.assertEqual(corpus.find_blocker("", fixture("quiet.log"), self.dead),
                         "exited (no error logged)")
        alive_blank = {"installed": True, "alive": True, "frame": False, "flat": None}
        self.assertEqual(corpus.find_blocker("", fixture("quiet.log"), alive_blank),
                         "no first frame (no error logged)")
        flat = {"installed": True, "alive": True, "frame": True, "flat": True}
        self.assertEqual(corpus.find_blocker("", fixture("quiet.log"), flat),
                         "flat screen (no error logged)")

    def test_a_working_app_has_no_blocker_even_with_errors_in_its_log(self):
        working = {"installed": True, "alive": True, "frame": True, "flat": False}
        self.assertEqual(corpus.find_blocker("", fixture("java-crash.log"), working), "-")

    def test_normalization_strips_addresses_pids_paths_and_numbers(self):
        a = corpus.normalize("11-05 17:42:39.077  1707  1712 F libc    : Fatal signal 11 (SIGSEGV), code 1 "
                             "(SEGV_MAPERR), fault addr 0x7f3a00001000 in tid 1712 (RenderThread), "
                             "pid 1707 (org.example.game)")
        b = corpus.normalize("Fatal signal 11 (SIGSEGV), code 1 (SEGV_MAPERR), fault addr 0x10 in tid 99 "
                             "(main), pid 99 (org.e)")
        self.assertEqual(a, b)
        self.assertEqual(a, "Fatal signal N (SIGSEGV), code N (SEGV_MAPERR), fault addr <addr> in tid N "
                            "(<thread>), pid N (<process>)")
        self.assertEqual(corpus.normalize("error: /samples/org.g_7.apk: its signature does not verify"),
                         "error: <path>: its signature does not verify")
        self.assertEqual(corpus.normalize("libGLESv1_CM.so needs libfoo2.so, which was not found"),
                         "libGLESv1_CM.so needs libfoo2.so, which was not found")
        self.assertEqual(corpus.normalize("Caused by: java.io.IOException: error=2, No such file"),
                         "Caused by: java.io.IOException: error=N, No such file")
        self.assertEqual(
            corpus.normalize("Unable to start activity ComponentInfo{org.a/org.a.Main}: boom"),
            "Unable to start activity ComponentInfo{<component>}: boom")

    def test_ranking_groups_by_signature_and_counts_apps(self):
        rows = corpus.read_report(os.path.join(FIXTURES, "report.tsv"))
        ranking = corpus.rank(rows)
        self.assertEqual(ranking[0], ("Caused by: java.security.KeyStoreException: AndroidKeyStore not found",
                                      ["org.b", "org.c", "org.f"]))
        self.assertEqual(ranking[1][1], ["org.d", "org.e"])
        self.assertEqual(len(ranking), 4)

    def test_rank_command_prints_counts_and_examples(self):
        out = subprocess.run(
            [sys.executable, "-I", os.path.join(HERE, "corpus.py"), "rank",
             os.path.join(FIXTURES, "report.tsv")],
            check=True, capture_output=True, text=True).stdout
        self.assertIn("Caused by: java.security.KeyStoreException: AndroidKeyStore not found: "
                      "3 apps (org.b, org.c, org.f)", out)
        self.assertIn("apps: 8", out)
        self.assertIn("installed: 7", out)
        self.assertIn("first frame: 2", out)
        self.assertIn("blocked: 7", out)


class Collect(Check):
    """What the sandbox writes is data from untrusted apps: the host copies only regular files with
    expected names out of it, never follows a link and never writes where the sandbox can."""

    def setUp(self):
        base = os.environ.get("TMPDIR", "/tmp")
        self.top = os.path.join(base, f"corpus-collect-{os.getpid()}-{id(self)}")
        self.stage, self.out = os.path.join(self.top, "stage"), os.path.join(self.top, "out")
        for sub in ("rows", "logs", "shots"):
            os.makedirs(os.path.join(self.stage, sub))
        os.makedirs(self.out)
        self.outside = os.path.join(self.top, "outside")
        with open(self.outside, "w") as f:
            f.write("untouched\n")

    def write(self, relative, text):
        with open(os.path.join(self.stage, relative), "w") as f:
            f.write(text)

    def row(self, package, blocker="-"):
        return "\t".join([package, "1", "x86_64", "yes", "100", "yes", "no", "yes", blocker])

    def test_rows_and_logs_of_the_run_are_copied(self):
        self.write("rows/b-0.tsv", self.row("org.a") + "\n")
        self.write("logs/org.a.log", "log of a\n")
        rows = corpus.collect(self.stage, self.out, {"org.a"})
        self.assertEqual(rows, [self.row("org.a")])
        with open(os.path.join(self.out, "report.tsv")) as f:
            self.assertEqual(f.read(), self.row("org.a") + "\n")
        with open(os.path.join(self.out, "logs", "org.a.log")) as f:
            self.assertEqual(f.read(), "log of a\n")
        self.assertFalse(os.path.exists(self.stage))

    def test_a_link_left_by_an_app_is_not_followed(self):
        os.symlink(self.outside, os.path.join(self.stage, "rows", "b-0.tsv"))
        os.symlink(self.outside, os.path.join(self.stage, "logs", "org.a.log"))
        os.rmdir(os.path.join(self.stage, "shots"))
        os.symlink(self.top, os.path.join(self.stage, "shots"))
        self.assertEqual(corpus.collect(self.stage, self.out, {"org.a"}), [])
        self.assertFalse(os.path.exists(os.path.join(self.out, "logs", "org.a.log")))
        with open(self.outside) as f:
            self.assertEqual(f.read(), "untouched\n")
        self.assertTrue(os.path.exists(self.top))

    def test_rows_naming_other_apps_or_with_extra_columns_are_dropped(self):
        self.write("rows/b-0.tsv", self.row("org.other") + "\n" + self.row("org.a") + "\tmore\n"
                   + self.row("org.a") + "\n")
        self.assertEqual(corpus.collect(self.stage, self.out, {"org.a"}), [self.row("org.a")])

    def test_the_report_is_not_written_through_a_link(self):
        os.symlink(self.outside, os.path.join(self.out, "report.tsv"))
        self.write("rows/b-0.tsv", self.row("org.a") + "\n")
        try:
            corpus.collect(self.stage, self.out, {"org.a"})
        except OSError:
            pass
        with open(self.outside) as f:
            self.assertEqual(f.read(), "untouched\n")

    def test_the_ranking_replaces_a_link_instead_of_writing_through_it(self):
        ranking = os.path.join(self.out, "ranking.txt")
        os.symlink(self.outside, ranking)
        corpus.replace_host(ranking, b"ranked\n")
        self.assertFalse(os.path.islink(ranking))
        with open(self.outside) as f:
            self.assertEqual(f.read(), "untouched\n")


if __name__ == "__main__":
    main()
