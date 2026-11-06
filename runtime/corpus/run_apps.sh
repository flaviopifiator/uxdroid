#!/bin/bash
# Runs inside the `sandbox` service, one app at a time: the apps that /samples/apps.tsv names (the
# TSV that `corpus.py select` prints), each from /samples/<package>_<version>.apk, under a headless
# compositor. For each it checks the first frame, that it is alive after WAIT seconds, that its
# screenshot is not one flat color and that a tap changes its UI, and appends one row to
# /out/rows/$SHARD.tsv; its logs go to /out/logs and its screenshot to /out/shots.
#
#   WAIT             seconds each app runs before the checks (default 60)
#   INSTALL_TIMEOUT  seconds an install may take (default 600)
#   SHARD            name of this runner's row file (default rows)
set -u -o pipefail
TARGET=/cache/target/release
CORPUS=(python3 -I /work/runtime/corpus/corpus.py)
WAIT="${WAIT:-60}"
INSTALL_TIMEOUT="${INSTALL_TIMEOUT:-600}"
SHARD="${SHARD:-rows}"
source /work/runtime/art/run.sh
WORK=$(mktemp -d)
source /work/runtime/art/test_lib.sh
source /work/runtime/gpu/compositor.sh
start_compositor
mkdir -p /out/rows /out/logs /out/shots
ROWS="/out/rows/$SHARD.tsv"

# Runs the uxdroid agent command for the app, bounded in time; its output goes to stdout.
agent() {
    timeout 30 "$TARGET/uxdroid" app "$package" "$@" 2>/dev/null
}

# The row ends with the app's framework, read from the libraries its install extracted.
row() {
    printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$package" "$version" "$abi" "$@" \
        "$("${CORPUS[@]}" framework "$ANDROID_DATA/data/$package/lib")" >> "$ROWS"
}

while IFS=$'\t' read -r -u 3 package version _ abi _ _ _; do
    apk="/samples/${package}_${version}.apk"
    [ -f "$apk" ] || continue
    APP=$(mktemp -d)
    export ANDROID_DATA="$APP/data" HOME="$APP/home" XDG_DATA_HOME="$APP/home/.local/share"
    mkdir -p "$ANDROID_DATA/dalvik-cache/$ARCH" "$HOME"
    log="/out/logs/$package.log"
    timeout "$INSTALL_TIMEOUT" "${RUNNER[@]}" "$TARGET/uxdroid" install "$apk" --data "$ANDROID_DATA" \
        >"$APP/install" 2>&1 </dev/null
    [ $? -eq 124 ] && echo "error: the install took longer than $INSTALL_TIMEOUT s" >> "$APP/install"
    { echo "== install"; cat "$APP/install"; } > "$log"
    if ! grep -q "^prepared $package in " "$APP/install"; then
        row no - - - - "$("${CORPUS[@]}" blocker "$APP/install" /dev/null no no no -)"
        rm -rf "$APP"
        echo "done $package: not installed"
        continue
    fi

    # Its own process group, so everything the app starts ends with it.
    setsid "${RUNNER[@]}" "$TARGET/uxdroid" service run "$package" --data-dir "$ANDROID_DATA" \
        >"$APP/run" 2>&1 </dev/null &
    app=$!
    alive=yes
    for _ in $(seq 1 "$WAIT"); do
        sleep 1
        kill -0 "$app" 2>/dev/null || { alive=no; break; }
    done

    flat=- tap=-
    if [ "$alive" = yes ]; then
        agent screenshot "$APP/a.png" >/dev/null
        flat=$("${CORPUS[@]}" flat "$APP/a.png")
        [ -f "$APP/a.png" ] && cp "$APP/a.png" "/out/shots/$package.png"
        agent ui > "$APP/ui-before.json"
        sleep 1
        agent screenshot "$APP/b.png" >/dev/null
        if target=$("${CORPUS[@]}" target "$APP/ui-before.json"); then
            read -r x y kind <<< "$target"
            if agent tap "$x" "$y" >/dev/null; then
                sleep 2
                agent ui > "$APP/ui-after.json"
                agent screenshot "$APP/after.png" >/dev/null
                tap=$("${CORPUS[@]}" changed "$APP/ui-before.json" "$APP/ui-after.json" \
                    "$APP/a.png" "$APP/b.png" "$APP/after.png")
                echo "tap at $x $y ($kind): changed $tap" >> "$APP/run"
            fi
        fi
        kill -0 "$app" 2>/dev/null || alive=no
    fi
    frame=$(sed -n 's/^uxdroid: first frame posted on .*, \([0-9]*\) ms after the framework started.*/\1/p' \
        "$APP/run" | head -1)
    has_frame=$([ -n "$frame" ] && echo yes || echo no)
    blocker=$("${CORPUS[@]}" blocker "$APP/install" "$APP/run" yes "$alive" "$has_frame" "$flat")
    row yes "${frame:--}" "$alive" "$flat" "$tap" "$blocker"
    { echo "== run"; cat "$APP/run"; } >> "$log"

    agent stop >/dev/null
    kill -TERM -- "-$app" 2>/dev/null
    sleep 1
    kill -KILL -- "-$app" 2>/dev/null
    wait "$app" 2>/dev/null
    rm -rf "$APP"
    echo "done $package: frame ${frame:--} alive $alive flat $flat tap $tap"
done 3< <(tail -n +2 /samples/apps.tsv)
