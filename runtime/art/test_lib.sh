# Shared by the smoke scripts: runs a headless compositor with an isolated home. Needs $WORK.
export XDG_RUNTIME_DIR="$WORK/run" HOME="$WORK/home" WAYLAND_DISPLAY=uxdroid-test UXDROID_RUNTIME="${RUNTIME:-/runtime/art}"
unset XDG_CACHE_HOME XDG_DATA_HOME
mkdir -p "$XDG_RUNTIME_DIR" "$HOME"
chmod 700 "$XDG_RUNTIME_DIR"

pause() { python3 -c "import time; time.sleep($1)"; }

start_compositor() {
    weston --backend=headless --renderer=pixman --socket="$WAYLAND_DISPLAY" --debug --idle-time=0 \
        --width=800 --height=600 >"$WORK/weston" 2>&1 &
    compositor=$!
    trap 'kill "$compositor" 2>/dev/null' EXIT
    for _ in $(seq 100); do
        [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && return
        pause 0.05
    done
    tail -5 "$WORK/weston"
    echo "FAIL: no compositor"
    exit 1
}
