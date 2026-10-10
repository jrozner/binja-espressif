#!/usr/bin/env bash
# Downloads the RISC-V GCC toolchain used by ESP-IDF, which builds the
# RISC-V test ELFs (scripts/build_test_elfs.sh). It is the same crosstool-NG
# release as the Xtensa toolchain of the xtensa disassembler's oracle
# (../xtensa/scripts/oracle/fetch-toolchain.sh), so both sets of test ELFs come
# from the same GCC.
#
# Version: esp-15.2.0_20251204, the toolchain of ESP-IDF v6.1 (tools/tools.json).
#
# Usage: scripts/fetch_riscv_toolchain.sh [DEST]   (default: ../xtensa/target/oracle)
set -euo pipefail

cd "$(dirname "$0")/.."
dest=${1:-../xtensa/target/oracle}
version=esp-15.2.0_20251204
url=https://github.com/espressif/crosstool-NG/releases/download/$version/riscv32-esp-elf-${version#esp-}-x86_64-linux-gnu.tar.xz
sha256=ace5aae6afe98f754947be043d40173e2e22ace57754b11a394b7238eefa01cf

if [[ -x "$dest/riscv32-esp-elf/bin/riscv32-esp-elf-gcc" ]] &&
    [[ "$(cat "$dest/riscv32-esp-elf/.version" 2>/dev/null)" == "$version" ]]; then
    exit 0
fi
mkdir -p "$dest"
rm -rf "$dest/riscv32-esp-elf"
archive="$dest/riscv32-esp-elf.tar.xz"
curl -sSLf --retry 3 -o "$archive" "$url"
echo "$sha256  $archive" | sha256sum -c -
tar -xJf "$archive" -C "$dest"
rm "$archive"
echo "$version" > "$dest/riscv32-esp-elf/.version"
