#!/usr/bin/env bash
# Builds tests/data/idf_<chip>.elf (ESP32, ESP32-S3, ESP32-C3, ESP32-C6)
# from tests/data/idf_features.c: ELFs with the layout quirks of ESP-IDF apps
# that the ELF fixups handle (see KNOWN_ISSUES.md). The linker scripts
# (tests/data/idf_<chip>.ld) put the switch's jump table in a writable
# `.flash.rodata` in a read-write PT_LOAD, and resolve the mask ROM functions
# (libgcc helpers included) and data the code uses to absolute symbols at
# their real ROM addresses. ROM_DATA names a ROM data symbol of each chip,
# which the code reads; ROM_FLASHCHIP selects the ESP32's flash chip
# variable over the later chips' pointer.
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
for chip in esp32 esp32s3; do
    case $chip in
        esp32) defs=(-DROM_DATA=_ctype_ -DROM_FLASHCHIP) ;;
        esp32s3) defs=(-DROM_DATA=g_coex_param_ptr) ;;
    esac
    export XTENSA_GNU_CONFIG=$toolchain/lib/xtensa_$chip.so
    "${prefix}gcc" -mdynconfig="$XTENSA_GNU_CONFIG" -O2 -ffreestanding -nostdlib -mlongcalls \
        "${defs[@]}" -c idf_features.c -o "$work/$chip.o"
    "${prefix}ld" -T "idf_$chip.ld" -o "idf_$chip.elf" "$work/$chip.o"
done

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
