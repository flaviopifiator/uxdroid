"""Tests for interact.py, against a fake `uxdroid` that answers from a script and logs each call.

    docker compose run --rm dev python3 runtime/corpus/test_interact.py
"""

import json
import os
import stat
import sys
import traceback

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import interact  # noqa: E402

# The fake answers `uxdroid app <package> <command> ...` with the first rule of answers.json whose
# command and, when given, text match, and logs every call to calls.log.
FAKE = """#!/usr/bin/env python3
import json, os, sys
here = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
with open(os.path.join(here, "calls.log"), "a") as log:
    log.write(" ".join(args[2:]) + "\\n")
with open(os.path.join(here, "answers.json")) as f:
    rules = json.load(f)
for rule in rules:
    if rule["command"] == args[2] and rule.get("text", args[-1]) in args:
        if rule.get("file"):
            open(args[3], "w").close()
        sys.stdout.write(rule.get("out", ""))
        sys.stderr.write(rule.get("err", ""))
        sys.exit(rule.get("code", 0))
sys.exit(0)
"""

NODE = {"node": {"bounds": [10, 20, 30, 40], "text": "Search", "visible": True}}


class Test:
    def setUp(self):
        # The dev image's Python is python3-minimal, which has no tempfile or shutil.
        base = os.environ.get("TMPDIR", "/tmp")
        self.dir = os.path.join(base, f"interact-{os.getpid()}-{id(self)}")
        os.makedirs(self.dir)
        self.program = os.path.join(self.dir, "uxdroid")
        with open(self.program, "w") as f:
            f.write(FAKE)
        os.chmod(self.program, stat.S_IRWXU)
        self.out = os.path.join(self.dir, "out")
        os.mkdir(self.out)
        self.answer([])

    def tearDown(self):
        for root, dirs, files in os.walk(self.dir, topdown=False):
            for name in files:
                os.remove(os.path.join(root, name))
            for name in dirs:
                os.rmdir(os.path.join(root, name))
        os.rmdir(self.dir)

    def answer(self, rules):
        with open(os.path.join(self.dir, "answers.json"), "w") as f:
            json.dump(rules, f)

    def calls(self):
        path = os.path.join(self.dir, "calls.log")
        if not os.path.exists(path):
            return []
        with open(path) as f:
            return f.read().splitlines()

    def play(self, steps):
        return interact.play(interact.App(self.program, "org.example", None), steps, self.out)

    def scenarios(self, value):
        path = os.path.join(self.dir, "scenarios.json")
        with open(path, "w") as f:
            json.dump(value, f)
        return path


def raises(function, *args):
    try:
        function(*args)
    except ValueError as error:
        return str(error)
    raise AssertionError("no ValueError")


class Scenarios(Test):
    def test_a_valid_file_loads(self):
        path = self.scenarios([{"package": "org.a", "steps": [["wait", "Hi", 5000], ["back"]]}])
        assert interact.load(path)[0]["package"] == "org.a"

    def test_unknown_actions_wrong_arguments_and_bad_shot_names_are_refused(self):
        for steps, problem in [
            ([["swipe", "x"]], "is not one of"),
            ([["tap"]], "takes 1 to 2 arguments"),
            ([["wait", "Hi", 0]], "the timeout must be 1 to 60000 ms"),
            ([["shot", "../x"]], "a shot name is"),
            ([], "has no steps"),
        ]:
            path = self.scenarios([{"package": "org.a", "steps": steps}])
            assert problem in raises(interact.load, path), (steps, raises(interact.load, path))

    def test_a_package_twice_is_refused(self):
        step = [["back"]]
        path = self.scenarios([{"package": "org.a", "steps": step}, {"package": "org.a", "steps": step}])
        assert raises(interact.load, path) == "org.a has two scenarios"


class Steps(Test):
    def test_a_tap_waits_for_the_text_then_taps_the_center_of_its_node(self):
        self.answer([{"command": "wait", "out": json.dumps(NODE)}])
        row = self.play([["tap", "Search", 3000]])
        assert row[1:5] == ["ok", "1", "-", "-"], row
        assert self.calls() == ["stop", "start", "wait --text Search --timeout 3000", "tap 20 30",
                                "status"], self.calls()

    def test_the_first_failing_step_ends_the_app_with_the_apps_reason(self):
        self.answer([{"command": "wait", "text": "Missing", "code": 1,
                      "err": "error: org.example refused: the condition did not hold within 5 ms (timeout)\n"}])
        row = self.play([["type", "hi"], ["wait", "Missing", 5], ["back"]])
        assert row[1:5] == ["fail", "2", "wait",
                            "error: org.example refused: the condition did not hold within 5 ms (timeout)"], row
        assert "back" not in self.calls()

    def test_an_app_that_died_is_reported_as_no_longer_running(self):
        self.answer([{"command": "key", "code": 1, "err": "error: cannot reach\n"},
                     {"command": "status", "code": 3, "err": "org.example is not running\n"}])
        row = self.play([["key", "ENTER"]])
        assert row[1:5] == ["fail", "1", "key", "error: cannot reach; the app is no longer running"], row

    def test_a_scenario_that_passes_but_leaves_the_app_dead_fails(self):
        self.answer([{"command": "status", "code": 3}])
        row = self.play([["back"]])
        assert row[1:5] == ["fail", "1", "status", "the app is no longer running"], row

    def test_absent_reads_the_tree_and_fails_while_the_text_is_shown(self):
        tree = {"windows": [{"root": {"visible": True, "children": [
            {"visible": True, "description": "No videos found."}]}}]}
        self.answer([{"command": "ui", "out": json.dumps(tree)}])
        assert self.play([["absent", "No videos"]])[1:5] == ["fail", "1", "absent",
                                                               "'No videos' is still shown"]
        assert self.play([["absent", "Retry"]])[1] == "ok"

    def test_a_shot_is_written_under_the_out_dir_by_package_and_name(self):
        self.answer([{"command": "screenshot", "file": True}])
        assert self.play([["shot", "home"]])[1] == "ok"
        assert os.path.exists(os.path.join(self.out, "org.example-home.png"))

    def test_a_failed_start_is_step_zero(self):
        self.answer([{"command": "start", "code": 1, "err": "error: no runtime\n"}])
        assert self.play([["back"]])[1:5] == ["fail", "0", "start", "error: no runtime"]


class Main(Test):
    def test_the_report_has_a_row_per_app_and_the_exit_code_says_whether_all_passed(self):
        path = self.scenarios([{"package": "org.a", "steps": [["back"]]},
                               {"package": "org.b", "steps": [["back"]]}])
        self.answer([{"command": "back", "text": "back", "code": 1, "err": "error: nope\n"}])
        code = interact.main([path, self.out, "--uxdroid", self.program, "--only", "org.b"])
        assert code == 1
        with open(os.path.join(self.out, "report.tsv")) as f:
            lines = f.read().splitlines()
        assert lines[0].split("\t") == interact.COLUMNS
        assert [line.split("\t")[:5] for line in lines[1:]] == [["org.b", "fail", "1", "back", "error: nope"]]

    def test_an_unknown_only_package_is_a_usage_error(self):
        path = self.scenarios([{"package": "org.a", "steps": [["back"]]}])
        assert interact.main([path, self.out, "--only", "org.zz"]) == 2


def main():
    failed = run = 0
    for case in [c for c in globals().values() if isinstance(c, type) and issubclass(c, Test) and c is not Test]:
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
            finally:
                test.tearDown()
    print(f"{run} tests, {failed} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
