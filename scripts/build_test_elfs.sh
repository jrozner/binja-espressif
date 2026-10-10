#!/usr/bin/env bash
# Builds tests/data/<chip>.elf from tests/data/corpus.c with Espressif's GCC.
#
# The Xtensa toolchain is the one the xtensa disassembler's oracle uses
# (xtensa-esp-elf esp-15.2.0_20251204; see ../xtensa/scripts/oracle), the
# RISC-V one is riscv32-esp-elf from the same release
# (scripts/fetch_riscv_toolchain.sh). The RISC-V ELFs also get
# tests/data/start_riscv.S: an ESP-IDF style call_start_cpu0 that sets gp, and
# a vectored interrupt table. Their linker scripts put __global_pointer$ at
# .sdata + 0x400 rather than ESP-IDF's + 0x800: ld only relaxes accesses to gp
# with a margin for section alignment, and the corpus's few small-data
# variables at the start of .sdata would otherwise be out of reach.
# External functions (memcpy, libgcc helpers) are resolved to distinct
# addresses past the code instead of linking libraries.
#
# Usage: scripts/build_test_elfs.sh [XTENSA_TOOLCHAIN_DIR [RISCV_TOOLCHAIN_DIR]]
set -euo pipefail
toolchain=$(realpath "${1:-../xtensa/target/oracle/xtensa-esp-elf}")
riscv=$(realpath "${2:-../xtensa/target/oracle/riscv32-esp-elf}")
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

prefix=$riscv/bin/riscv32-esp-elf-
for chip in esp32c3 esp32c6; do
    case $chip in
        esp32c3) march=rv32imc_zicsr_zifencei ;;
        esp32c6) march=rv32imac_zicsr_zifencei ;;
    esac
    flags=(-march=$march -mabi=ilp32 -O2 -ffreestanding -nostdlib)
    "${prefix}gcc" "${flags[@]}" -c corpus.c -o "$work/$chip.o"
    "${prefix}gcc" "${flags[@]}" -c start_riscv.S -o "$work/$chip.start.o"
    base=$(grep -oP '^\s*\.text \K0x[0-9a-f]+' "$chip.ld")
    defs=() addr=$((base + 0x10000))
    for sym in $("${prefix}nm" -u "$work/$chip.o" | awk '{print $2}'); do
        defs+=("--defsym=$sym=$addr")
        addr=$((addr + 16))
    done
    # ld only relaxes (gp-relative small data, deleting alignment padding)
    # when asked; the gcc driver ESP-IDF links with always asks.
    "${prefix}ld" --relax -T "$chip.ld" "${defs[@]}" -o "$chip.elf" "$work/$chip.start.o" "$work/$chip.o"
done
