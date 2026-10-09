#!/usr/bin/env bash
# Builds tests/data/<chip>.elf from tests/data/corpus.c with Espressif's GCC.
#
# The toolchain is the one the xtensa disassembler's oracle uses
# (xtensa-esp-elf esp-15.2.0_20251204; see ../xtensa/scripts/oracle).
# External functions (memcpy, libgcc helpers) are resolved to distinct
# addresses past the code instead of linking libraries.
#
# Usage: scripts/build_test_elfs.sh [TOOLCHAIN_DIR]
set -euo pipefail
toolchain=$(realpath "${1:-../xtensa/target/oracle/xtensa-esp-elf}")
prefix=$toolchain/bin/xtensa-esp-elf-
cd "$(dirname "$0")/../tests/data"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for chip in esp32 esp32s2 esp32s3 esp8266; do
    export XTENSA_GNU_CONFIG=$toolchain/lib/xtensa_$chip.so
    "${prefix}gcc" -mdynconfig="$XTENSA_GNU_CONFIG" -O2 -ffreestanding -nostdlib -mlongcalls -c corpus.c -o "$work/$chip.o"
    base=$(grep -oP '\.text \K0x[0-9a-f]+' "$chip.ld")
    defs=() addr=$((base + 0x10000))
    for sym in $("${prefix}nm" -u "$work/$chip.o" | awk '{print $2}'); do
        defs+=("--defsym=$sym=$addr")
        addr=$((addr + 16))
    done
    "${prefix}ld" -T "$chip.ld" "${defs[@]}" -o "$chip.elf" "$work/$chip.o"
done
