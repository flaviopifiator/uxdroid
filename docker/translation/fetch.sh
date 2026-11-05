#!/bin/sh
# Downloads each source of a list (destination, URL, SHA-256), checks it and unpacks it.
# Usage: fetch.sh <list> <root>
set -eu
list=$1
root=$2
grep -v '^#' "$list" | while read -r dest url sum; do
    [ -n "$dest" ] || continue
    archive=$(mktemp)
    curl -fsSL -o "$archive" "$url"
    echo "$sum  $archive" | sha256sum -c - >/dev/null
    mkdir -p "$root/$dest"
    tar -xzf "$archive" -C "$root/$dest" --strip-components=1
    rm -f "$archive"
done
