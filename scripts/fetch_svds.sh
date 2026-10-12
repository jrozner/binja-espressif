#!/usr/bin/env bash
# Downloads the SVD files vendored in view/espressif/svd/ (and Espressif's license for them)
# from the repository and commit view/espressif/svd/SOURCES pins each to, and checks them
# against the SHA-256 recorded there.
#
# espressif/svd is no longer maintained and has no ESP32-C5 file; the C5's
# comes from espressif/esp-bist (the file carries Espressif's Apache-2.0
# license text; the rest of that repository is LGPL-3.0 and is not used).
# To move a file to a newer commit, edit its line in view/espressif/svd/SOURCES, run this
# with --update to print the new SHA-256s, record them, and review the diff.
#
# Usage: scripts/fetch_svds.sh [--update] [FILE...]
#   FILE      only these files of view/espressif/svd/SOURCES (default: all)
#   --update  report SHA-256 mismatches instead of failing on them
set -euo pipefail

update=
if [ "${1-}" = --update ]; then
    update=1
    shift
fi
dir=$(cd "$(dirname "$0")/../view/espressif/svd" && pwd)

sha256() { (sha256sum "$1" 2>/dev/null || shasum -a 256 "$1") | cut -d' ' -f1; }

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
status=0
found=0
while read -r file repo commit path sha; do
    case "$file" in '' | '#'*) continue ;; esac
    if [ $# -gt 0 ]; then
        wanted=
        for f in "$@"; do [ "$f" = "$file" ] && wanted=1; done
        [ -n "$wanted" ] || continue
    fi
    found=$((found + 1))
    url=https://raw.githubusercontent.com/$repo/$commit/$path
    curl -sSfL --retry 3 -o "$tmp" "$url"
    actual=$(sha256 "$tmp")
    if [ "$actual" != "$sha" ]; then
        if [ -z "$update" ]; then
            echo "$file: SHA-256 mismatch for $url: expected $sha, got $actual" >&2
            status=1
            continue
        fi
        echo "$file: new SHA-256 $actual (record it in view/espressif/svd/SOURCES)"
    fi
    case "$file" in
        *.svd)
            grep -q 'Licensed under the Apache License, Version 2.0' "$tmp" || {
                echo "$file: $url lacks the Apache-2.0 license text" >&2
                status=1
                continue
            }
            ;;
    esac
    cat "$tmp" >"$dir/$file"
    echo "$file: $url"
done < "$dir/SOURCES"
if [ $# -gt 0 ] && [ "$found" -ne $# ]; then
    echo "not all of $* are in view/espressif/svd/SOURCES" >&2
    status=1
fi
exit $status
