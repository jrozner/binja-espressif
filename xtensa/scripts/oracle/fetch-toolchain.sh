#!/usr/bin/env bash
# Downloads the Xtensa GCC toolchain used by ESP-IDF, whose binutils serve as
# the reference disassembler. It is a single `xtensa-esp-elf` toolchain that
# targets a specific chip through a dynamic configuration library
# (lib/xtensa_<chip>.so), selected with the XTENSA_GNU_CONFIG environment
# variable (as, objdump, ld, ...) or -mdynconfig (gcc).
#
# Version: esp-15.2.0_20251204, the toolchain of ESP-IDF v6.1 (tools/tools.json).
#
# Usage: scripts/oracle/fetch-toolchain.sh [DEST]   (default: target/oracle)
set -euo pipefail

dest=${1:-target/oracle}
version=esp-15.2.0_20251204
url=https://github.com/espressif/crosstool-NG/releases/download/$version/xtensa-esp-elf-${version#esp-}-x86_64-linux-gnu.tar.xz
sha256=3d50f5cd5f173acfd524e07c1cd69bc99585731a415ca2e5bce879997fe602b8

if [[ -x "$dest/xtensa-esp-elf/bin/xtensa-esp-elf-objdump" ]] &&
    [[ "$(cat "$dest/xtensa-esp-elf/.version" 2>/dev/null)" == "$version" ]]; then
    exit 0
fi
mkdir -p "$dest"
rm -rf "$dest/xtensa-esp-elf"
archive="$dest/xtensa-esp-elf.tar.xz"
curl -sSLf --retry 3 -o "$archive" "$url"
echo "$sha256  $archive" | sha256sum -c -
tar -xJf "$archive" -C "$dest"
rm "$archive"
echo "$version" > "$dest/xtensa-esp-elf/.version"
