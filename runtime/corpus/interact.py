"""Drives installed apps through user flows written as scenarios, and reports which step failed.

One file and stdlib only, like corpus.py, so `python3 -I` runs it in the containers.

    python3 -I runtime/corpus/interact.py <scenarios.json> <out dir> [--only <package>]...
        [--uxdroid <program>] [--data-dir <dir>]

A scenario is an app and its steps, each a list whose first word is the action:

    ["wait", <text>, <ms>?]    a visible node's text, description or hint holds <text>
    ["tap", <text>, <ms>?]     wait for it, then tap the center of that node
    ["type", <text>]           type into the focused field
    ["key", <name>]            a KeyEvent.KEYCODE_ name, such as ENTER
    ["back"]                   the back key
    ["idle"]                   no window drew for 500 ms
    ["absent", <text>]         no visible node holds <text> now
    ["shot", <name>]           screenshot to <out>/<package>-<name>.png

Each app is stopped, started and taken through its steps; the first step that fails ends it, and an
app that is no longer running at the end fails too. <out>/report.tsv gets one row per app.
"""

import json
import os
import subprocess
import sys
import time

WAIT_MS = 20000
COLUMNS = ["package", "result", "step", "action", "reason", "ms"]
ACTIONS = {"wait": (1, 2), "tap": (1, 2), "type": (1, 1), "key": (1, 1), "back": (0, 0),
           "idle": (0, 0), "absent": (1, 1), "shot": (1, 1)}


class StepFailed(Exception):
    pass


def load(path):
    """The scenarios of a file, checked before any app is touched; ValueError names the problem."""
    with open(path, encoding="utf-8") as f:
        scenarios = json.load(f)
    if not isinstance(scenarios, list):
        raise ValueError("the scenarios must be a list")
    seen = set()
    for i, scenario in enumerate(scenarios):
        where = f"scenario {i + 1}"
        if not isinstance(scenario, dict) or not isinstance(scenario.get("package"), str):
            raise ValueError(f"{where} has no package")
        package = scenario["package"]
        if package in seen:
            raise ValueError(f"{package} has two scenarios")
        seen.add(package)
        steps = scenario.get("steps")
        if not isinstance(steps, list) or not steps:
            raise ValueError(f"{package} has no steps")
        for n, step in enumerate(steps, 1):
            check_step(step, f"{package} step {n}")
    return scenarios


def check_step(step, where):
    if not isinstance(step, list) or not step or step[0] not in ACTIONS:
        raise ValueError(f"{where} is not one of {', '.join(sorted(ACTIONS))}: {step!r}")
    least, most = ACTIONS[step[0]]
    arguments = step[1:]
    if not least <= len(arguments) <= most:
        raise ValueError(f"{where}: {step[0]} takes {least} to {most} arguments: {step!r}")
    if least and not (isinstance(arguments[0], str) and arguments[0]):
        raise ValueError(f"{where}: {step[0]} needs text: {step!r}")
    if len(arguments) == 2 and not (isinstance(arguments[1], int) and 0 < arguments[1] <= 60000):
        raise ValueError(f"{where}: the timeout must be 1 to 60000 ms: {step!r}")
    if step[0] == "shot" and not all(c.isalnum() or c in "-_" for c in arguments[0]):
        raise ValueError(f"{where}: a shot name is letters, digits, - and _: {step!r}")


def center(node):
    bounds = node.get("bounds") if isinstance(node, dict) else None
    if not (isinstance(bounds, list) and len(bounds) == 4 and all(isinstance(v, int) for v in bounds)):
        return None
    left, top, right, bottom = bounds
    if right <= left or bottom <= top:
        return None
    return ((left + right) // 2, (top + bottom) // 2)


def holds(tree, text):
    """Whether a visible node of `ui`'s tree has `text` in its text, description or hint."""
    def visit(node):
        if not isinstance(node, dict):
            return False
        if node.get("visible") is True and any(
                isinstance(node.get(k), str) and text in node[k] for k in ("text", "description", "hint")):
            return True
        return any(visit(c) for c in node.get("children") or [])
    return any(visit(w.get("root")) for w in tree.get("windows") or [] if isinstance(w, dict))


class App:
    def __init__(self, program, package, data_dir):
        self.program, self.package, self.data_dir = program, package, data_dir

    def run(self, *args, timeout=90):
        command = [self.program, "app", self.package, *args]
        try:
            done = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            raise StepFailed(f"`{' '.join(args)}` did not answer within {timeout} s")
        if done.returncode != 0:
            last = (done.stderr.strip() or done.stdout.strip()).splitlines()
            raise StepFailed(last[-1] if last else f"`{' '.join(args)}` exited {done.returncode}")
        return done.stdout

    def start(self):
        try:
            self.run("stop")
        except StepFailed:
            pass
        extra = ["--data-dir", self.data_dir] if self.data_dir else []
        self.run("start", *extra)

    def step(self, step, out):
        action, arguments = step[0], step[1:]
        if action in ("wait", "tap"):
            ms = arguments[1] if len(arguments) == 2 else WAIT_MS
            answer = self.run("wait", "--text", arguments[0], "--timeout", str(ms), timeout=ms // 1000 + 30)
            if action == "tap":
                try:
                    point = center(json.loads(answer).get("node"))
                except ValueError:
                    point = None
                if point is None:
                    raise StepFailed(f"the node holding {arguments[0]!r} has no area to tap")
                self.run("tap", str(point[0]), str(point[1]))
        elif action == "type":
            self.run("type", arguments[0])
        elif action == "key":
            self.run("key", arguments[0])
        elif action == "back":
            self.run("back")
        elif action == "idle":
            self.run("wait", "--idle", "--timeout", str(WAIT_MS), timeout=WAIT_MS // 1000 + 30)
        elif action == "absent":
            try:
                tree = json.loads(self.run("ui"))
            except ValueError:
                raise StepFailed("ui did not answer with JSON")
            if holds(tree, arguments[0]):
                raise StepFailed(f"{arguments[0]!r} is still shown")
        elif action == "shot":
            self.run("screenshot", os.path.join(out, f"{self.package}-{arguments[0]}.png"))

    def alive(self):
        try:
            self.run("status")
            return True
        except StepFailed:
            return False


def play(app, steps, out):
    """One report row: ok, or the first step that failed and why."""
    started = time.monotonic()

    def row(result, number, action, reason):
        ms = int((time.monotonic() - started) * 1000)
        return [app.package, result, str(number), action, reason, str(ms)]

    try:
        app.start()
    except StepFailed as failure:
        return row("fail", 0, "start", str(failure))
    for number, step in enumerate(steps, 1):
        try:
            app.step(step, out)
        except StepFailed as failure:
            if not app.alive():
                return row("fail", number, step[0], f"{failure}; the app is no longer running")
            return row("fail", number, step[0], str(failure))
    if not app.alive():
        return row("fail", len(steps), "status", "the app is no longer running")
    return row("ok", len(steps), "-", "-")


def tsv(fields):
    return "\t".join(f.replace("\t", " ").replace("\n", " ") for f in fields)


def main(argv):
    args, only, program, data_dir = [], set(), "uxdroid", None
    i = 0
    while i < len(argv):
        if argv[i] in ("--only", "--uxdroid", "--data-dir") and i + 1 < len(argv):
            value = argv[i + 1]
            if argv[i] == "--only":
                only.add(value)
            elif argv[i] == "--uxdroid":
                program = value
            else:
                data_dir = value
            i += 2
        else:
            args.append(argv[i])
            i += 1
    if len(args) != 2:
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    try:
        scenarios = load(args[0])
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    unknown = only - {s["package"] for s in scenarios}
    if unknown:
        print(f"error: no scenario for {', '.join(sorted(unknown))}", file=sys.stderr)
        return 2
    out = args[1]
    os.makedirs(out, exist_ok=True)
    rows = []
    for scenario in scenarios:
        if only and scenario["package"] not in only:
            continue
        app = App(program, scenario["package"], data_dir)
        rows.append(play(app, scenario["steps"], out))
        print(tsv(rows[-1]), flush=True)
    with open(os.path.join(out, "report.tsv"), "w", encoding="utf-8") as f:
        f.write("\n".join(tsv(r) for r in [COLUMNS, *rows]) + "\n")
    passed = sum(r[1] == "ok" for r in rows)
    print(f"{passed} of {len(rows)} apps passed")
    return 0 if passed == len(rows) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
