# Sourced by smoke scripts after runtime/art/test_lib.sh: with RENDERER set, such as RENDERER=gl in
# dev-gpu, the headless compositor renders with it, and weston's GL renderer imports dmabufs, which
# its default pixman renderer does not.
if [ -n "${RENDERER:-}" ]; then
    start_compositor() {
        weston --backend=headless --renderer="$RENDERER" --socket="$WAYLAND_DISPLAY" --debug --idle-time=0 \
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
fi
