#!/usr/bin/env bash
# Builds tests/data/idf_<chip>.elf from tests/data/idf_features.c: ELFs with
# the layout quirks of ESP-IDF apps that the ELF fixups handle (see
# KNOWN_ISSUES.md). The linker scripts (tests/data/idf_<chip>.ld) put the
# switch's jump table in a writable `.flash.rodata` in a read-write PT_LOAD,
# and resolve the mask ROM functions and data the code uses to absolute
# symbols at their real ROM addresses. ROM_DATA names a ROM data symbol of
# each chip, which the code reads.
#
# Uses the same toolchains as scripts/build_test_elfs.sh.
#
# Usage: scripts/build_idf_elfs.sh [XTENSA_TOOLCHAIN_DIR [RISCV_TOOLCHAIN_DIR]]
set -euo pipefail
toolchain=$(realpath "${1:-../xtensa/target/oracle/xtensa-esp-elf}")
riscv=$(realpath "${2:-../xtensa/target/oracle/riscv32-esp-elf}")
cd "$(dirname "$0")/../tests/data"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

prefix=$toolchain/bin/xtensa-esp-elf-
export XTENSA_GNU_CONFIG=$toolchain/lib/xtensa_esp32.so
"${prefix}gcc" -mdynconfig="$XTENSA_GNU_CONFIG" -O2 -ffreestanding -nostdlib -mlongcalls \
    -DROM_DATA=_ctype_ -c idf_features.c -o "$work/esp32.o"
"${prefix}ld" -T idf_esp32.ld -o idf_esp32.elf "$work/esp32.o"

prefix=$riscv/bin/riscv32-esp-elf-
for chip in esp32c3 esp32c6; do
    case $chip in
        esp32c3) march=rv32imc_zicsr_zifencei rom_data=g_coex_param_ptr ;;
        esp32c6) march=rv32imac_zicsr_zifencei rom_data=crc8_be_table_ptr ;;
    esac
    flags=(-march=$march -mabi=ilp32 -O2 -ffreestanding -nostdlib)
    "${prefix}gcc" "${flags[@]}" -DROM_DATA=$rom_data -c idf_features.c -o "$work/$chip.o"
    "${prefix}gcc" "${flags[@]}" -c start_riscv.S -o "$work/$chip.start.o"
    # As in build_test_elfs.sh: relax, as the gcc driver does.
    "${prefix}ld" --relax -T "idf_$chip.ld" -o "idf_$chip.elf" "$work/$chip.start.o" "$work/$chip.o"
done
