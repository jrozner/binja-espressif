#!/usr/bin/env bash
# Downloads the SVD files vendored in view/espressif/svd/ (and the licenses for
# them) from the repository and commit view/espressif/svd/SOURCES pins each to,
# regenerates the ones made with svdtools, and checks them all against the
# SHA-256 recorded there.
#
# espressif/svd is no longer maintained and has no ESP32-C5 file; the C5's
# comes from espressif/esp-bist (the file carries Espressif's Apache-2.0
# license text; the rest of that repository is LGPL-3.0 and is not used).
# Espressif never published one for the ESP8266: esp8266.svd is made from
# esp-rs/esp-pacs' file and patches (the esp-pacs/ lines, kept in a temporary
# directory only) and view/espressif/svd/esp8266.yaml with svdtools, the
# version its SOURCES lines name (cargo install svdtools --version <version>
# --locked; $SVDTOOLS overrides the svdtools on the PATH).
#
# To move a file to a newer commit, edit its line in view/espressif/svd/SOURCES,
# run this with --update to print the new SHA-256s, record them, and review
# the diff. The same goes for a change to esp8266.yaml.
#
# Usage: scripts/fetch_svds.sh [--update] [FILE...]
#   FILE      only these files of view/espressif/svd/SOURCES (default: all;
#             a generated file brings the esp-pacs/ inputs with it)
#   --update  report SHA-256 mismatches instead of failing on them
set -euo pipefail

update=
if [ "${1-}" = --update ]; then
    update=1
    shift
fi
dir=$(cd "$(dirname "$0")/../view/espressif/svd" && pwd)
svdtools=${SVDTOOLS:-svdtools}

sha256() { (sha256sum "$1" 2>/dev/null || shasum -a 256 "$1") | cut -d' ' -f1; }

# Downloads and svdtools' input and output go here; a file named with a
# directory (esp-pacs/...) stays here, the others are copied to $dir once
# they check out.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

wanted() {
    [ $# -gt 1 ] || return 0
    local file=$1
    shift
    for f in "$@"; do [ "$f" = "$file" ] && return 0; done
    return 1
}

# The svdtools inputs are needed when a file made with svdtools is, and so
# is the svdtools version those lines name.
inputs=
while read -r file repo commit _; do
    case "$file" in '' | '#'*) continue ;; esac
    if [ "$repo" = svdtools ] && wanted "$file" "$@"; then inputs=$commit; fi
done < "$dir/SOURCES"
if [ -n "$inputs" ]; then
    version=$("$svdtools" --version 2>/dev/null) || version=
    if [ "$version" != "svdtools $inputs" ]; then
        echo "needs svdtools $inputs, found ${version:-none} (cargo install svdtools --version $inputs --locked)" >&2
        exit 1
    fi
fi

status=0
found=0
while read -r file repo commit path sha; do
    case "$file" in '' | '#'*) continue ;; esac
    case "$file" in
        */*) [ -n "$inputs" ] || continue ;;
        *)
            wanted "$file" "$@" || continue
            found=$((found + 1))
            ;;
    esac
    out=$work/$file
    mkdir -p "$(dirname "$out")"
    if [ "$repo" = svdtools ]; then
        # $commit is the svdtools version (checked above), $path the patch
        # file: in $dir unless it is one of the downloads.
        case "$path" in */*) ;; *) cp "$dir/$path" "$work/$path" ;; esac
        log=$work/svdtools.log
        if ! "$svdtools" patch "$work/$path" "$out" >"$log" 2>&1; then
            cat "$log" >&2
            echo "$file: svdtools patch $path failed" >&2
            exit 1
        fi
        source="svdtools $commit patch $path"
    else
        source=https://raw.githubusercontent.com/$repo/$commit/$path
        curl -sSfL --retry 3 -o "$out" "$source"
    fi
    actual=$(sha256 "$out")
    if [ "$actual" != "$sha" ]; then
        if [ -z "$update" ]; then
            echo "$file: SHA-256 mismatch for $source: expected $sha, got $actual" >&2
            status=1
            continue
        fi
        echo "$file: new SHA-256 $actual (record it in view/espressif/svd/SOURCES)"
    fi
    case "$file" in
        */*)
            echo "$file: $source (temporary)"
            continue
            ;;
        *.svd)
            # Espressif's files carry the Apache-2.0 text; the generated one
            # names esp-pacs' licenses (LICENSE-esp-pacs-*).
            if [ "$repo" = svdtools ]; then
                notice='Copyright The esp-rs Project Developers, licensed under'
            else
                notice='Licensed under the Apache License, Version 2.0'
            fi
            grep -q "$notice" "$out" || {
                echo "$file: $source lacks the license notice \"$notice\"" >&2
                status=1
                continue
            }
            ;;
    esac
    cat "$out" >"$dir/$file"
    echo "$file: $source"
done < "$dir/SOURCES"
if [ $# -gt 0 ] && [ "$found" -ne $# ]; then
    echo "not all of $* are in view/espressif/svd/SOURCES" >&2
    status=1
fi
exit $status
