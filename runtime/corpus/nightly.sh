#!/bin/bash
# Runs the F-Droid catalog unattended: refreshes uxdroid's verified copy of the F-Droid index, picks
# the apps to run, then in batches downloads them (checked against the index), runs each in the
# `sandbox` service with run_apps.sh, and ranks what stopped them. See docs/corpus.md.
#
# usage: runtime/corpus/nightly.sh [--sample <n>] [--seed <s>] [--jobs <n>] [--wait <s>]
#                                  [--batch <n>] [--catalog <dir>] [--out <dir>] [--keep]
#
# Resumable: run it again with the same --out and it skips the apps report.tsv already has.
set -u -o pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT" || exit 1
CORPUS=(python3 -I "$ROOT/runtime/corpus/corpus.py")

SAMPLE="" SEED=0 JOBS=3 WAIT=60 BATCH="" KEEP=no
CATALOG="${UXDROID_CATALOG:-$HOME/uxdroid-samples/fdroid-catalog}"
OUT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --sample) SAMPLE="$2"; shift 2 ;;
        --seed) SEED="$2"; shift 2 ;;
        --jobs) JOBS="$2"; shift 2 ;;
        --wait) WAIT="$2"; shift 2 ;;
        --batch) BATCH="$2"; shift 2 ;;
        --catalog) CATALOG="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --keep) KEEP=yes; shift ;;
        *) sed -n '6,7p' "$0" >&2; exit 2 ;;
    esac
done
BATCH="${BATCH:-$((JOBS * 4))}"
OUT="${OUT:-$CATALOG-out/$(date +%Y-%m-%d)}"
CATALOG=$(mkdir -p "$CATALOG" && cd "$CATALOG" && pwd)
OUT=$(mkdir -p "$OUT" && cd "$OUT" && pwd)
APKS="$CATALOG/apks" SHARDS="$CATALOG/run"
mkdir -p "$CATALOG/store" "$APKS" "$SHARDS" "$OUT/batches" "$OUT/rows"

exec 9>"$OUT/.lock"
flock -n 9 || { echo "another run is using $OUT" >&2; exit 1; }
trap 'kill $(jobs -p) 2>/dev/null' EXIT
say() { echo "$(date '+%F %T') $*" | tee -a "$OUT/run.log"; }

say "building uxdroid"
docker compose run --rm -T dev cargo build --release -p uxdroid-cli >"$OUT/build.log" 2>&1 \
    || { tail -20 "$OUT/build.log"; exit 1; }

# arm64-v8a code runs through Dynarmic, which the `translation` image builds; the sandbox gets a copy.
say "copying the arm64 translator"
mkdir -p "$CATALOG/translator"
docker compose run --rm -T -v "$CATALOG/translator:/translator" translation \
    cp /opt/uxdroid/lib/libuxdroid_dynarmic.so /translator/ >"$OUT/translator.log" 2>&1 \
    || say "no translator (docker compose build translation): arm64-v8a apps will fail to load their code"

# uxdroid's store verifies entry.jar against F-Droid's pinned key and the index against the entry's
# SHA-256 before it keeps the index; only that verified copy is read here.
say "refreshing the F-Droid index"
docker compose run --rm -T -v "$CATALOG/store:/store" dev \
    /cache/target/release/uxdroid search --refresh --limit 1 --data /store fdroid >/dev/null \
    || { echo "the index refresh failed" >&2; exit 1; }
INDEX="$CATALOG/store/store/sources/fdroid/index.json"

if [ ! -f "$OUT/apps.tsv" ]; then
    "${CORPUS[@]}" select "$INDEX" ${SAMPLE:+--sample "$SAMPLE" --seed "$SEED"} \
        >"$OUT/apps.tsv.part" 2> >(tee -a "$OUT/run.log" >&2) && mv "$OUT/apps.tsv.part" "$OUT/apps.tsv"
fi
[ -f "$OUT/report.tsv" ] || printf "package\tversion\tabi\tinstalled\tfirst_frame_ms\talive\tflat\ttap_changed\tblocker\n" \
    >"$OUT/report.tsv"
# Rows of a run that stopped half way through a batch are kept.
cat "$OUT"/rows/*.tsv >>"$OUT/report.tsv" 2>/dev/null; rm -f "$OUT"/rows/*.tsv

"${CORPUS[@]}" pending "$OUT/apps.tsv" "$OUT/report.tsv" >"$OUT/pending.tsv"
rm -f "$OUT"/batches/*.tsv
header=$(head -1 "$OUT/pending.tsv")
tail -n +2 "$OUT/pending.tsv" | split -l "$BATCH" -d -a 5 --additional-suffix=.tsv - "$OUT/batches/"
for file in "$OUT"/batches/*.tsv; do
    [ -f "$file" ] && sed -i "1i $header" "$file"
done
batches=("$OUT"/batches/*.tsv)
[ -f "${batches[0]}" ] || batches=()
say "$(($(wc -l <"$OUT/pending.tsv") - 1)) apps to run in ${#batches[@]} batches, $JOBS at a time, $WAIT s each"

download() {
    "${CORPUS[@]}" download "$1" "$APKS" >"$1.download" 2>&1
    awk -F'\t' '$2 != "ok"' "$1.download" >>"$OUT/download-errors.tsv"
}

run_batch() {
    local list="$1" name j package
    name=$(basename "$list" .tsv)
    for j in $(seq 0 $((JOBS - 1))); do
        mkdir -p "$SHARDS/$name-$j"
        head -1 "$list" >"$SHARDS/$name-$j/apps.tsv"
    done
    j=0
    while IFS=$'\t' read -r package _ ; do
        local row file
        row=$(awk -F'\t' -v p="$package" '$1 == p' "$list")
        file="${package}_$(cut -f2 <<<"$row").apk"
        [ -f "$APKS/$file" ] || continue
        ln -f "$APKS/$file" "$SHARDS/$name-$((j % JOBS))/$file"
        echo "$row" >>"$SHARDS/$name-$((j % JOBS))/apps.tsv"
        j=$((j + 1))
    done < <(awk -F'\t' '$2 == "ok"' "$list.download")
    for j in $(seq 0 $((JOBS - 1))); do
        [ "$(wc -l <"$SHARDS/$name-$j/apps.tsv")" -gt 1 ] || continue
        UXDROID_SAMPLES="$SHARDS/$name-$j" docker compose run --rm -T -e WAIT="$WAIT" -e SHARD="$name-$j" \
            -e UXDROID_TRANSLATOR=/translator/libuxdroid_dynarmic.so -v "$CATALOG/translator:/translator:ro" \
            -v "$OUT:/out" sandbox bash /work/runtime/corpus/run_apps.sh >"$OUT/batches/$name-$j.log" 2>&1 &
    done
    wait $(jobs -p | grep -v -x "${downloading:-none}")
    cat "$OUT"/rows/"$name"-*.tsv >>"$OUT/report.tsv" 2>/dev/null
    rm -f "$OUT"/rows/"$name"-*.tsv
    for j in $(seq 0 $((JOBS - 1))); do
        rm -f "$SHARDS/$name-$j"/*.apk "$SHARDS/$name-$j/apps.tsv"
        rmdir "$SHARDS/$name-$j"
    done
    if [ "$KEEP" = no ]; then
        tail -n +2 "$list" | while IFS=$'\t' read -r package version _; do
            rm -f "$APKS/${package}_$version.apk"
        done
    fi
    "${CORPUS[@]}" rank "$OUT/report.tsv" >"$OUT/ranking.txt"
}

# The next batch downloads while this one runs.
if [ ${#batches[@]} -gt 0 ]; then
    download "${batches[0]}"
fi
for i in "${!batches[@]}"; do
    downloading=""
    if [ $((i + 1)) -lt ${#batches[@]} ]; then
        download "${batches[$((i + 1))]}" &
        downloading=$!
    fi
    say "batch $((i + 1)) of ${#batches[@]}"
    run_batch "${batches[$i]}"
    [ -n "$downloading" ] && wait "$downloading"
done
"${CORPUS[@]}" rank "$OUT/report.tsv" >"$OUT/ranking.txt"
say "done: $(($(wc -l <"$OUT/report.tsv") - 1)) apps in report.tsv; ranking in $OUT/ranking.txt"
