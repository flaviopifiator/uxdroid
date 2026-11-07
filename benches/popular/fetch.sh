#!/bin/bash
# Downloads the popular corpus (manifest.tsv) and verifies every file before keeping it.
#
# usage: benches/popular/fetch.sh [--dir <dir>]      (default ~/uxdroid-samples/popular)
#
# Direct rows: HTTPS only, redirects included, each file kept only when its SHA-256 is the manifest's.
# A `TOFU` row (no published checksum) is pinned at its first download: the hash is written to
# <dir>/pins.tsv and must be copied into manifest.tsv. IzzyOnDroid rows: uxdroid's own store refreshes
# and verifies the index in `dev` (the only container with network; no APK is opened there), then
# runtime/corpus/corpus.py downloads each APK checked against the index's size and SHA-256.
# Writes <dir>/fetched.tsv: id, variant, file, size, sha256, url, hash.
set -u -o pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
HERE="$ROOT/benches/popular"
DIR="${POPULAR_DIR:-$HOME/uxdroid-samples/popular}"
[ "${1:-}" = --dir ] && DIR="$2"
IZZY_URL=https://apt.izzysoft.de/fdroid/repo
# Published at https://apt.izzysoft.de/fdroid/index/info
IZZY_FINGERPRINT=3BF0D6ABFEAE2F401707B6D966BE743BF0EEE49C2561B9BA39073711F628937A
TARGET=/cache/target-popular/release
APKS="$DIR/apks"
mkdir -p "$APKS" "$DIR/store"
cd "$ROOT" || exit 1
FETCHED="$DIR/fetched.tsv"
printf "id\tvariant\tfile\tsize\tsha256\turl\thash\n" >"$FETCHED"
failed=0

manifest() { grep -v '^#' "$HERE/manifest.tsv" | tail -n +2; }

# Direct downloads.
while IFS=$'\t' read -r id _ source variant url sha256 hash; do
    [ "$source" = direct ] || continue
    file="${id}_${variant}.apk"
    if [ ! -f "$APKS/$file" ] || [ "$(sha256sum "$APKS/$file" | cut -d' ' -f1)" != "$sha256" ]; then
        rm -f "$APKS/$file.part"
        if ! curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL --retry 2 -o "$APKS/$file.part" "$url"; then
            echo "fail $id ($variant): download from $url" >&2; failed=1; rm -f "$APKS/$file.part"; continue
        fi
        got=$(sha256sum "$APKS/$file.part" | cut -d' ' -f1)
        if [ "$sha256" = TOFU ]; then
            printf "%s\t%s\t%s\n" "$id" "$variant" "$got" >>"$DIR/pins.tsv"
            echo "pinned $id ($variant) at first download: $got (copy it into manifest.tsv)" >&2
        elif [ "$got" != "$sha256" ]; then
            echo "fail $id ($variant): SHA-256 $got, expected $sha256; not kept" >&2
            rm -f "$APKS/$file.part"; failed=1; continue
        fi
        mv "$APKS/$file.part" "$APKS/$file"
    fi
    got=$(sha256sum "$APKS/$file" | cut -d' ' -f1)
    printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$id" "$variant" "$file" "$(stat -c %s "$APKS/$file")" "$got" "$url" "$hash" >>"$FETCHED"
    echo "ok $id ($variant)"
done < <(manifest)

# IzzyOnDroid, through uxdroid's store.
if manifest | awk -F'\t' '$3 == "izzy"' | grep -q .; then
    docker compose run --rm -T -e CARGO_TARGET_DIR=/cache/target-popular dev cargo build --release -q \
        -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-system -p uxdroid-platform \
        || { echo "the build failed" >&2; exit 1; }
    docker compose run --rm -T -v "$DIR/store:/store" dev sh -c "
        $TARGET/uxdroid sources --data /store 2>/dev/null | grep -q '^izzy ' \
            || $TARGET/uxdroid sources add izzy $IZZY_URL --fingerprint $IZZY_FINGERPRINT --data /store
        $TARGET/uxdroid search --refresh --limit 1 --data /store izzy >/dev/null" \
        || { echo "the IzzyOnDroid index refresh failed" >&2; exit 1; }
    index="$DIR/store/store/sources/izzy/index.json"
    python3 -I runtime/corpus/corpus.py select "$index" >"$DIR/izzy-all.tsv" 2>/dev/null
    { head -1 "$DIR/izzy-all.tsv"
      manifest | awk -F'\t' '$3 == "izzy" {print $5}' | while read -r package; do
          awk -F'\t' -v p="$package" '$1 == p' "$DIR/izzy-all.tsv"
      done; } >"$DIR/izzy.tsv"
    python3 -I runtime/corpus/corpus.py download "$DIR/izzy.tsv" "$APKS" --repo "$IZZY_URL" >"$DIR/izzy.download"
    while IFS=$'\t' read -r id _ source _ package _ hash; do
        [ "$source" = izzy ] || continue
        row=$(awk -F'\t' -v p="$package" '$1 == p' "$DIR/izzy.tsv")
        status=$(awk -F'\t' -v p="$package" '$1 == p {print $2 "\t" $3}' "$DIR/izzy.download")
        if [ -z "$row" ] || [ "${status%%$'\t'*}" != ok ]; then
            echo "fail $id: ${status:-no release in the index that fits SDK 36 and x86_64 or arm64-v8a}" >&2
            failed=1; continue
        fi
        file="${package}_$(cut -f2 <<<"$row").apk"
        printf "%s\tauto\t%s\t%s\t%s\t%s\t%s\n" "$id" "$file" "$(cut -f7 <<<"$row")" "$(cut -f6 <<<"$row")" \
            "$IZZY_URL$(cut -f5 <<<"$row")" "$hash" >>"$FETCHED"
        echo "ok $id"
    done < <(manifest)
fi
exit $failed
