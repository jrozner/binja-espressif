#!/usr/bin/env bash
# Compiles tests/corpus/*.c with Espressif's GCC and records objdump's
# disassembly of the result as test fixtures:
#
#   tests/data/corpus/<isa>-<source>-<opt>.bin   raw .text contents
#   tests/data/corpus/<isa>-<source>-<opt>.tsv   objdump listing (sweep.py format)
#
# The code is linked at each chip's flash-mapped instruction address so that
# branch, call and literal addresses look like real firmware.
#
# Requires the toolchain from fetch-toolchain.sh and python3.
#
# Usage: scripts/oracle/build_corpus.sh [TOOLCHAIN_DIR]   (default: target/oracle/xtensa-esp-elf)
set -euo pipefail
# nm sorts symbols with strcoll, and the order of `nm -u` decides the
# addresses given to external functions, so the output depends on the locale.
export LC_ALL=C

toolchain=$(realpath "${1:-target/oracle/xtensa-esp-elf}")
prefix=$toolchain/bin/xtensa-esp-elf-
out=tests/data/corpus
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
rm -f "$out"/*

build() {
    local isa=$1 base=$2
    shift 2
    # Selects the chip for binutils; gcc takes it as -mdynconfig.
    local config=$toolchain/lib/xtensa_$isa.so
    export XTENSA_GNU_CONFIG=$config
    for src in "$@"; do
        local name
        name=$(basename "$src" .c)
        for opt in O2 Os; do
            local stem="$isa-$name-$opt" elf="$work/$isa-$name-$opt.elf"
            "${prefix}gcc" -mdynconfig="$config" -"$opt" -mlongcalls -ffreestanding -nostdlib -c "$src" -o "$work/$stem.o"
            # Resolve external functions (libgcc helpers, extern declarations)
            # to distinct addresses near the code instead of linking libraries.
            local defsyms=() addr=$((base + 0x10000))
            for sym in $("${prefix}nm" -u "$work/$stem.o" | awk '{print $2}'); do
                defsyms+=("--defsym=$sym=$addr")
                addr=$((addr + 0x40))
            done
            "${prefix}ld" -Ttext="$base" -e 0 "${defsyms[@]}" "$work/$stem.o" -o "$elf"
            "${prefix}objcopy" -O binary --only-section=.text "$elf" "$out/$stem.bin"
            "${prefix}objdump" -d -j .text "$elf" | python3 -I -c '
import re, sys
for line in sys.stdin:
    m = re.match(r"^\s*([0-9a-f]+):\t([0-9a-f]+)\s*\t(.*)$", line.rstrip("\n"))
    if m:
        word, text = m.group(2), m.group(3).replace("\t", " ").strip()
        # Literal pool words are listed without a mnemonic; they are data.
        if not text:
            continue
        print("%s\t%s\t%d\t%s" % (word, m.group(1), len(word) // 2, text.split(" <", 1)[0]))
' > "$out/$stem.tsv"
        done
    done
}

common=(tests/corpus/integer.c tests/corpus/float.c tests/corpus/system.c)
build esp32 0x400d0000 "${common[@]}" tests/corpus/esp32_only.c
build esp32s2 0x40080000 "${common[@]}" tests/corpus/esp32s2_only.c
build esp32s3 0x42000000 "${common[@]}" tests/corpus/esp32s3_only.c
build esp8266 0x40201000 "${common[@]}"
