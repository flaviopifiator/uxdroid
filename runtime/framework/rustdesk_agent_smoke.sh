#!/bin/bash
# Drives RustDesk's x86_64 build (Flutter) the way an agent would, through uxdroid app: finds the
# Remote ID field in ui by its hint, types an ID into it and waits for Flutter to show it, finds the
# Settings tab by its description, taps it, and reads back what ui reports of the Settings page.
# Flutter reports its widgets through its accessibility bridge, which turns on only while an agent
# asks. Its native libraries are untrusted, so this runs only in the sandbox service:
#
#   docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
#   UXDROID_SAMPLES=<directory with the APK> docker compose run --rm sandbox \
#       bash /work/runtime/framework/rustdesk_agent_smoke.sh
#
# SHOTS=<writable directory> keeps the screenshots, as PNG.
set -u -o pipefail
WORK=$(mktemp -d)
# Removed however the script ends, a failure before the full trap below included.
trap 'rm -rf "$WORK"' EXIT
APK="${RUSTDESK_APK:-/samples/rustdesk-1.5.1-universal-signed.apk}"
PACKAGE=com.carriez.flutter_hbb
fail() {
    echo "FAIL: $*"
    [ -f "$WORK/out" ] && head -c 1500 "$WORK/out" && echo
    [ -f "$WORK/app" ] && grep -v -E "jni_internal|^	at " "$WORK/app" | tail -"${TAIL:-20}" | cut -c1-220
    exit 1
}

TARGET="${CARGO_TARGET_DIR:-/cache/target}/release"
for file in uxdroid uxdroid-keymap libuxdroid_system.so libuxdroid_nativebridge.so; do
    [ -e "$TARGET/$file" ] || fail "no $TARGET/$file: build the release libraries in the dev service first"
done
[ -r "$APK" ] || fail "no APK at $APK: set UXDROID_SAMPLES to its directory, or RUSTDESK_APK"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
"$TARGET/uxdroid" install "$APK" --data "$ANDROID_DATA" >"$WORK/install" 2>&1 || { cat "$WORK/install"; fail "install"; }

source /work/runtime/art/test_lib.sh
mkfifo "$WORK/fifo"
"$TARGET/uxdroid-keymap" us >"$WORK/keymap" || fail "no us keymap"
python3 /work/runtime/framework/input_compositor.py "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" "$WORK/fifo" \
    "$WORK/keymap" >"$WORK/compositor" 2>&1 &
compositor=$!
for _ in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; pause 0.05; done
export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
"$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
    -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" "$APK" >"$WORK/app" 2>&1 &
app=$!
trap 'kill "$app" "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT

agent() { "$TARGET/uxdroid" app "$PACKAGE" "$@" >"$WORK/out" 2>&1; }
# The id of the first visible node whose $1 (text, description or hint) starts with $2.
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
        if node.get("visible") and str(node.get(sys.argv[2], "")).startswith(sys.argv[3]):
            print(node["id"])
            sys.exit(0)
sys.exit(1)
EOF
}
shot() {
    agent screenshot "$WORK/$1.png" || fail "screenshot: exit $?"
    [ -n "${SHOTS:-}" ] && cp "$WORK/$1.png" "$SHOTS/"
    return 0
}

for _ in $(seq 600); do
    grep -q "^uxdroid: first frame posted on $PACKAGE/" "$WORK/app" && agent status && break
    kill -0 "$app" 2>/dev/null || fail "RustDesk exited"
    pause 0.1
done
agent status || fail "RustDesk serves no control socket"
agent wait --idle --timeout 30000 || fail "RustDesk never went idle"
echo "stage: RustDesk's home screen is drawn and serves its control socket"

# 1. Flutter's widgets, through its accessibility bridge.
agent ui || fail "ui: exit $?"
grep -q '"accessibility":true' "$WORK/out" || fail "accessibility is not on while the agent asks"
FIELD=$(node hint "Remote ID") || fail "ui has no Remote ID field"
SETTINGS=$(node description "Settings") || fail "ui has no Settings tab"
NODES=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["nodes"])' "$WORK/out")
echo "ok: ui reports $NODES nodes of Flutter's tree: the Remote ID field (node $FIELD) and the Settings tab (node $SETTINGS)"
shot home

# 2. A Remote ID typed into the field, as key presses; Flutter groups the digits as it draws them.
agent type 123456789 --node "$FIELD" || fail "type: exit $?"
grep -q "^typed 9 characters: 9 keys, 0 committed$" "$WORK/out" || fail "type: $(cat "$WORK/out")"
agent wait --text "123 456 789" --timeout 10000 || fail "Flutter never showed the typed ID"
python3 -c 'import json,sys; n=json.load(open(sys.argv[1]))["node"]; assert n.get("editable"), n; print("ok: Flutter shows the typed Remote ID in", n["class"], repr(n["text"]))' "$WORK/out" \
    || fail "the typed ID is not in the field"
shot typed

# 3. The Settings tab, found by its description and tapped, opens another page.
agent ui || fail "ui: exit $?"
SETTINGS=$(node description "Settings") || fail "ui lost the Settings tab"
agent tap --node "$SETTINGS" || fail "tap: exit $?"
agent wait --idle --timeout 10000 || fail "not idle after the tap"
agent ui || fail "ui after Settings: exit $?"
cp "$WORK/out" "$WORK/settings.json"
python3 - "$WORK/settings.json" <<'EOF' || fail "ui does not show the Settings page"
import json, sys
snapshot = json.load(open(sys.argv[1]))
def walk(node):
    yield node
    for child in node.get("children", []):
        yield from walk(child)
labels = []
for window in snapshot["windows"]:
    for node in walk(window.get("root", {})):
        label = node.get("text") or node.get("description")
        if node.get("visible") and label:
            labels.append(label.replace("\n", " / "))
selected = [n for w in snapshot["windows"] for n in walk(w.get("root", {})) if n.get("selected")]
assert any(n.get("description", "").startswith("Settings") for n in selected), selected
assert not any(label.startswith("Oops, no recent sessions") for label in labels), labels
print(f"ok: the Settings tab is selected and ui reports {len(labels)} labelled nodes:")
for label in labels[:40]:
    print("  ui:", label[:100])
EOF
shot settings

agent stop || fail "stop: exit $?"
echo "ok: an agent reads and drives RustDesk's Flutter widgets through uxdroid app"
