#!/bin/bash
# Runs the popular corpus that fetch.sh downloaded, with the F-Droid catalog's runner,
# runtime/corpus/run_apps.sh, in the `sandbox` service (no network, read-only), and writes one report.
#
# usage: benches/popular/run.sh [--jobs <n>] [--wait <s>] [--dir <dir>] [--out <dir>] [--only <id,...>]
#
# Two passes, one after the other so they do not share the CPU: `native` runs every app once, with its
# x86_64 code when it has any, else its arm64-v8a code through the translator; `translated` runs the
# arm64-v8a builds of the apps the manifest lists per ABI. Within a pass, --jobs sandbox containers
# (default 3) run apps at once. Beside run_apps.sh, sample_cpu.py records each app's CPU time and
# memory every second. Output in --out (default <dir>/out/<date>): report.tsv, ranking.txt,
# logs/<pass>/<package>.log, shots/<pass>/<package>.png, cpu/<pass>/<package>.tsv.
set -u -o pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
HERE="$ROOT/benches/popular"
cd "$ROOT" || exit 1
JOBS=3 WAIT=60 ONLY=""
DIR="${POPULAR_DIR:-$HOME/uxdroid-samples/popular}" OUT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --jobs) JOBS="$2"; shift 2 ;;
        --wait) WAIT="$2"; shift 2 ;;
        --dir) DIR="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --only) ONLY=",$2,"; shift 2 ;;
        *) sed -n '5p' "$0" >&2; exit 2 ;;
    esac
done
DIR=$(cd "$DIR" && pwd) || exit 1
OUT="${OUT:-$DIR/out/$(date +%Y-%m-%d)}"
OUT=$(mkdir -p "$OUT" && cd "$OUT" && pwd)
TARGET=/cache/target-popular/release
say() { echo "$(date '+%F %T') $*" | tee -a "$OUT/run.log"; }
[ -f "$DIR/fetched.tsv" ] || { echo "no $DIR/fetched.tsv: run benches/popular/fetch.sh first" >&2; exit 1; }

say "building uxdroid into $TARGET"
docker compose run --rm -T -e CARGO_TARGET_DIR=/cache/target-popular dev cargo build --release \
    -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-system -p uxdroid-platform >"$OUT/build.log" 2>&1 \
    || { tail -20 "$OUT/build.log"; exit 1; }
git -C "$ROOT" rev-parse HEAD >"$OUT/commit.txt"
git -C "$ROOT" status --short >>"$OUT/commit.txt"

# The arm64 engine comes from the `translation` image; a copy is mounted read-only in the sandbox.
say "copying the arm64 translator"
mkdir -p "$DIR/translator"
for attempt in 1 2 3 4 5 6; do
    docker compose run --rm -T -v "$DIR/translator:/translator" translation \
        cp /opt/uxdroid/lib/libuxdroid_dynarmic.so /translator/ >"$OUT/translator.log" 2>&1 && break
    say "the translation image is not available (attempt $attempt); retrying in 60 s"
    sleep 60
done
{ docker image inspect uxdroid-translation --format 'image {{.Id}} built {{.Created}}'
  sha256sum "$DIR/translator/libuxdroid_dynarmic.so"; } >"$OUT/translator.txt" 2>&1

# What each APK is: package, versionCode and native ABIs, read by uxdroid in the sandbox.
say "inspecting the APKs"
UXDROID_SAMPLES="$DIR/apks" docker compose run --rm -T sandbox bash -c "
    for f in /samples/*.apk; do
        printf '%s\t' \"\${f##*/}\"
        $TARGET/uxdroid inspect \"\$f\" 2>&1 | awk '
            /^package: /{p=\$2} /^version: /{v=\$NF; gsub(/[()]/, \"\", v); n=\$2} /^abis: /{sub(/^abis: /, \"\"); a=\$0}
            END {printf \"%s\t%s\t%s\t%s\n\", p, v, n, a}'
    done" >"$OUT/inspect.tsv" 2>"$OUT/inspect.log"

# The apps.tsv of each pass, in the columns run_apps.sh reads (corpus.py select's).
python3 -I "$HERE/report.py" plan "$HERE/manifest.tsv" "$DIR/fetched.tsv" "$OUT/inspect.tsv" "$OUT" "$ONLY" \
    || exit 1

run_pass() {
    local pass="$1" list="$OUT/plan-$1.tsv" j row package version file
    [ "$(wc -l <"$list")" -gt 1 ] || return 0
    mkdir -p "$OUT/stage/$pass" "$DIR/run"
    for j in $(seq 0 $((JOBS - 1))); do
        rm -rf "${DIR:?}/run/$pass-$j" "$OUT/stage/$pass/$j"
        mkdir -p "$DIR/run/$pass-$j" "$OUT/stage/$pass/$j"
        head -1 "$list" >"$DIR/run/$pass-$j/apps.tsv"
    done
    j=0
    while IFS= read -r row; do
        package=$(cut -f1 <<<"$row") version=$(cut -f2 <<<"$row") file=$(cut -f5 <<<"$row")
        ln -f "$DIR/apks/$file" "$DIR/run/$pass-$((j % JOBS))/${package}_$version.apk"
        echo "$row" >>"$DIR/run/$pass-$((j % JOBS))/apps.tsv"
        j=$((j + 1))
    done < <(tail -n +2 "$list")
    say "pass $pass: $j apps, $JOBS at a time, $WAIT s each"
    for j in $(seq 0 $((JOBS - 1))); do
        [ "$(wc -l <"$DIR/run/$pass-$j/apps.tsv")" -gt 1 ] || continue
        # run_apps.sh runs unchanged but for its build directory, this corpus's own.
        UXDROID_SAMPLES="$DIR/run/$pass-$j" docker compose run --rm -T -e WAIT="$WAIT" -e SHARD="$pass-$j" \
            -e UXDROID_TRANSLATOR=/translator/libuxdroid_dynarmic.so -v "$DIR/translator:/translator:ro" \
            -v "$OUT/stage/$pass/$j:/out" sandbox bash -c "
                python3 -I /work/benches/popular/sample_cpu.py /out/cpu & sampler=\$!
                sed 's#^TARGET=/cache/target/release\$#TARGET=$TARGET#' /work/runtime/corpus/run_apps.sh >/tmp/run_apps.sh
                bash /tmp/run_apps.sh
                kill \$sampler" >"$OUT/stage/$pass/$j.log" 2>&1 &
    done
    wait
    for j in $(seq 0 $((JOBS - 1))); do
        rm -rf "${DIR:?}/run/$pass-$j"
    done
}

run_pass native
run_pass translated
python3 -I "$HERE/report.py" collect "$OUT" || exit 1
python3 -I runtime/corpus/corpus.py rank "$OUT/corpus-report.tsv" >"$OUT/ranking.txt"
say "done: $OUT/report.tsv, $OUT/ranking.txt"
