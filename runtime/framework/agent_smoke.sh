#!/bin/bash
# Drives an SDK-built APK the way an agent would, through uxdroid app and uxdroid mcp: finds a button
# by its text in ui, taps it, types into an EditText, waits for the typed text, takes a screenshot and
# checks it against the compositor's own frame, and does the same through the MCP server. A scripted
# compositor (input_compositor.py) plays the display. Runs in the dev service; the APK is ours, built
# by build_system.sh. RUNS=<n> times ui and screenshot calls n times each.
set -u -o pipefail
WORK=$(mktemp -d)
PACKAGE=com.example.input
fail() {
    echo "FAIL: $*"
    for f in "$WORK/out" "$LOG"; do [ -f "$f" ] && tail -"${TAIL:-25}" "$f" | cut -c1-220; done
    [ -f "$WORK/compositor" ] && tail -8 "$WORK/compositor" | cut -c1-160
    exit 1
}

cargo build -q --release -p uxdroid-system -p uxdroid-cli --manifest-path /work/Cargo.toml || fail "build"
TARGET="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
source /work/runtime/art/test_lib.sh
LOG="$XDG_RUNTIME_DIR/uxdroid/$PACKAGE.log"
mkfifo "$WORK/fifo"
"$TARGET/uxdroid-keymap" us >"$WORK/keymap" || fail "no us keymap"
python3 /work/runtime/framework/input_compositor.py "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" "$WORK/fifo" \
    "$WORK/keymap" >"$WORK/compositor" 2>&1 &
compositor=$!
trap '"$TARGET/uxdroid" app "$PACKAGE" stop >/dev/null 2>&1; kill "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT
for _ in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; pause 0.05; done
UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "$RUNTIME/samples/framework/input.apk" --data "$ANDROID_DATA" \
    >"$WORK/install" 2>&1 || { cat "$WORK/install"; fail "install"; }

app() { "$TARGET/uxdroid" app "$PACKAGE" "$@" >"$WORK/out" 2>"$WORK/err"; }
logged() {
    for _ in $(seq 200); do
        grep -q -E "$1" "$LOG" && return
        pause 0.05
    done
    fail "$2"
}
# The id of the first node in the snapshot in $WORK/out whose class ends with $1 and whose text is $2.
node() {
    python3 - "$WORK/out" "$1" "$2" <<'EOF'
import json, sys
snapshot = json.load(open(sys.argv[1]))
def walk(node):
    yield node
    for child in node.get("children", []):
        yield from walk(child)
for window in snapshot["windows"]:
    for node in walk(window.get("root", {})):
        if node.get("class", "").endswith(sys.argv[2]) and node.get("text") == sys.argv[3]:
            print(node["id"])
            sys.exit(0)
sys.exit(1)
EOF
}

app start --data-dir "$ANDROID_DATA" || fail "start: exit $?"
logged "^input: window focus true$" "window focus"
logged "^input: layout field " "a layout"
"$TARGET/uxdroid" app "$PACKAGE" wait --idle --timeout 10000 >/dev/null || fail "the app never went idle"

# 1. The screen as structured data: the button and the field, with text and bounds.
app ui || fail "ui: exit $?"
BUTTON=$(node .Button TAP) || fail "ui has no Button with text TAP: $(head -c 600 "$WORK/out")"
FIELD=$(node .EditText "Type here") || fail "ui has no EditText showing its hint"
read -r _ _ _ BX BY < <(grep "^input: layout button " "$LOG" | tail -1)
python3 - "$WORK/out" "$BUTTON" "$BX" "$BY" <<'EOF' || fail "the button's bounds are not where the app laid it out"
import json, sys
snapshot = json.load(open(sys.argv[1]))
def walk(node):
    yield node
    for child in node.get("children", []):
        yield from walk(child)
node = next(n for w in snapshot["windows"] for n in walk(w["root"]) if n["id"] == int(sys.argv[2]))
left, top, right, bottom = node["bounds"]
assert ((left + right) // 2, (top + bottom) // 2) == (int(sys.argv[3]), int(sys.argv[4])), node
assert node.get("clickable") and node.get("visible"), node
EOF
echo "ok: ui reports the Button (node $BUTTON) and the EditText (node $FIELD) with their bounds"

# 2. A tap on the node, through the seat: a finger, at the node's center.
app tap --node "$BUTTON" || fail "tap: exit $? $(cat "$WORK/err")"
grep -q "^tapped node $BUTTON at $BX,$BY$" "$WORK/out" || fail "tap did not report the node's center: $(cat "$WORK/out")"
logged "^input: clicked 1$" "a click from the agent's tap"
grep -q "^input: motion ACTION_DOWN source 0x1002 at $BX,$BY$" "$LOG" || fail "the tap was not a touchscreen event"
echo "ok: a tap on node $BUTTON reached the Button as a finger"

# 3. Typing into the field, as key presses of the compositor's keymap.
app type "Hi agent 2!" --node "$FIELD" || fail "type: exit $? $(cat "$WORK/err")"
grep -q "^typed 11 characters: 11 keys, 0 committed$" "$WORK/out" || fail "type: $(cat "$WORK/out")"
logged '^input: text "Hi agent 2!"$' "the typed text in the EditText"
grep -q "^input: key KEYCODE_H char 72 meta 0x41$" "$LOG" || fail "H was not typed with shift"
app wait --text "Hi agent 2!" --timeout 5000 || fail "wait: exit $? $(cat "$WORK/err")"
python3 -c 'import json,sys; n=json.load(open(sys.argv[1]))["node"]; assert n["class"].endswith("EditText"), n' "$WORK/out" \
    || fail "wait did not return the EditText"
echo "ok: typed text reached the EditText, and wait found it"

# 4. Keys and back.
app key DEL || fail "key: exit $?"
logged '^input: text "Hi agent 2"$' "DEL deleting a character"
app scroll 200 400 --dy 2 || fail "scroll: exit $?"
app back || fail "back: exit $?"
grep -q "^pressed BACK$" "$WORK/out" || fail "back did not report"
echo "ok: key DEL, scroll and back"

# 5. The screenshot is the frame the compositor shows, pixel for pixel; the tap turned it green.
"$TARGET/uxdroid" app "$PACKAGE" wait --idle >/dev/null || fail "not idle"
app screenshot "$WORK/shot.png" || fail "screenshot: exit $? $(cat "$WORK/err")"
grep -q "^wrote $WORK/shot.png (400x700)$" "$WORK/out" || fail "screenshot: $(cat "$WORK/out")"
echo "shot $WORK/shot.ppm" >"$WORK/fifo"
for _ in $(seq 100); do grep -q "shot $WORK/shot.ppm" "$WORK/compositor" && break; pause 0.05; done
python3 - "$WORK/shot.png" "$WORK/shot.ppm" <<'EOF' || fail "the screenshot differs from the compositor's frame"
import struct, sys, zlib
data = open(sys.argv[1], "rb").read()
assert data[:8] == b"\x89PNG\r\n\x1a\n"
at, idat = 8, b""
while at < len(data):
    length, kind = struct.unpack(">I4s", data[at:at + 8])
    body = data[at + 8:at + 8 + length]
    if kind == b"IHDR":
        width, height = struct.unpack(">II", body[:8])
    elif kind == b"IDAT":
        idat += body
    at += 12 + length
raw = zlib.decompress(idat)
row = width * 3
png = b"".join(raw[y * (row + 1) + 1:(y + 1) * (row + 1)] for y in range(height))
header, ppm = open(sys.argv[2], "rb").read().split(b"\n", 1)
assert header.split()[1:3] == [str(width).encode(), str(height).encode()], header
assert png == ppm, sum(1 for i in range(0, len(png), 3) if png[i:i + 3] != ppm[i:i + 3])
green = sum(1 for i in range(0, len(png), 3) if png[i:i + 3] == bytes((0x66, 0xBB, 0x6A)))
assert green > 10000, green
print(f"ok: the screenshot equals the compositor's frame, {green} pixels green after the tap")
EOF

# 6. The same through the MCP server: initialize, the tools, ui, tap and an image.
python3 - "$TARGET/uxdroid" "$PACKAGE" "$BUTTON" <<'EOF' || fail "the MCP server"
import json, subprocess, sys
server = subprocess.Popen([sys.argv[1], "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
def call(id, method, params):
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id, "method": method, "params": params}) + "\n")
    server.stdin.flush()
    answer = json.loads(server.stdout.readline())
    assert answer["id"] == id and "result" in answer, answer
    return answer["result"]
call(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}})
server.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
names = [tool["name"] for tool in call(2, "tools/list", {})["tools"]]
assert "ui" in names and "screenshot" in names, names
tool = lambda id, name, **arguments: call(id, "tools/call", {"name": name, "arguments": {"package": sys.argv[2], **arguments}})
listed = call(3, "tools/call", {"name": "list", "arguments": {}})
assert sys.argv[2] in listed["content"][0]["text"], listed
ui = tool(4, "ui")
assert not ui["isError"] and '"TAP"' in ui["content"][0]["text"], ui
tap = tool(5, "tap", node=int(sys.argv[3]))
assert not tap["isError"], tap
shot = tool(6, "screenshot", max_size=200)
assert shot["content"][0]["type"] == "image" and shot["content"][0]["mimeType"] == "image/png", shot["content"][1]
bad = tool(7, "tap", x=1)
assert bad["isError"], bad
server.stdin.close()
assert server.wait(10) == 0
print("ok: the MCP server lists the tools and drives the app: ui, tap, screenshot as an image, errors as tool errors")
EOF
logged "^input: clicked 2$" "a click from the MCP tap"

# 7. What a call costs, on one connection, as the MCP server keeps one.
python3 - "$XDG_RUNTIME_DIR/uxdroid/$PACKAGE.sock" "${RUNS:-20}" <<'EOF'
import json, socket, sys, time
median = lambda values: sorted(values)[len(values) // 2]
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
f = s.makefile("rwb")
def call(cmd, **fields):
    f.write((json.dumps({"v": 1, "cmd": cmd, **fields}) + "\n").encode())
    f.flush()
    started = time.perf_counter()
    line = f.readline()
    assert json.loads(line)["ok"], line[:200]
    return (time.perf_counter() - started) * 1000, len(line)
first, size = call("ui")
for cmd in ("ui", "screenshot", "tap"):
    fields = {"x": 10, "y": 680} if cmd == "tap" else {}
    times = [call(cmd, **fields) for _ in range(int(sys.argv[2]))]
    print(f"cost: {cmd} median {median([t for t, _ in times]):.2f} ms, max {max(t for t, _ in times):.2f} ms, "
          f"{times[0][1]} bytes, {len(times)} calls")
print(f"cost: first ui on a connection (accessibility turning on) {first:.1f} ms")
EOF
[ $? -eq 0 ] || fail "timing the calls"

app stop || fail "stop: exit $?"
echo "ok: an agent reads and drives an app through uxdroid app and uxdroid mcp"
