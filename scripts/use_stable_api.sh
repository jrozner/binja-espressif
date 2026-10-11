#!/usr/bin/env bash
# Points binaryninja and binaryninjacore-sys at the stable binaryninja-api
# release, in place of the dev revision Cargo.toml pins on main, and updates
# Cargo.lock to match. CI's stable-api job and the release workflow run this
# before building; STABLE_TAG below is the one place the stable release is
# named.
#
# A native plugin loads only in a Binary Ninja whose core ABI matches the API
# it was built against, so release builds, for the stable Binary Ninja that
# most users run, have to be built against the stable API. Cargo cannot
# [patch] a git dependency onto another revision of the same repository, so
# the pins are rewritten instead. Don't commit the result.
#
# Usage: scripts/use_stable_api.sh
set -euo pipefail

STABLE_TAG="stable/6.0.10601"

cd "$(dirname "$0")/.."

# The two dependency lines as main has them: the API repository at a `rev`.
pin='^(binaryninja|binaryninjacore-sys) = \{ git = "https://github\.com/Vector35/binaryninja-api\.git", rev = "[0-9a-f]+"'

matches=$(grep -cE "$pin" Cargo.toml || true)
if [ "$matches" -ne 2 ]; then
  echo "error: expected 2 binaryninja-api rev pins in Cargo.toml, found $matches" >&2
  exit 1
fi

# Portable between GNU and BSD sed: no -i, and `#` delimits, as the pattern
# and the tag contain `/`.
sed -E "\#$pin#s#rev = \"[0-9a-f]+\"#tag = \"$STABLE_TAG\"#" Cargo.toml > Cargo.toml.stable
mv Cargo.toml.stable Cargo.toml

matches=$(grep -cF "tag = \"$STABLE_TAG\"" Cargo.toml || true)
if [ "$matches" -ne 2 ]; then
  echo "error: rewrote $matches binaryninja-api pins to $STABLE_TAG, expected 2" >&2
  exit 1
fi

cargo update -p binaryninja -p binaryninjacore-sys
echo "binaryninja and binaryninjacore-sys now use binaryninja-api $STABLE_TAG"
