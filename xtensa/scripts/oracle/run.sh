#!/usr/bin/env bash
# Verifies the decoder against GNU objdump for every instruction encoding of
# every supported core, then regenerates every test fixture derived from
# objdump:
#
#   tests/data/<isa>.digest   digest of the (verified) decoder output
#   tests/data/<isa>.tsv      sampled objdump output
#   tests/data/corpus/        compiled code and objdump listings
#
# Fails if the decoder disagrees with objdump on any encoding. The nightly CI
# workflow runs this and then checks that the committed fixtures are unchanged. The sweep output
# (tens of gigabytes for the ESP32-S3) is streamed, never stored.
#
# Requires curl, python3 and an x86-64 Linux host for the toolchain.
set -euo pipefail
cd "$(dirname "$0")/../.."

work=target/oracle
toolchain=$work/xtensa-esp-elf
scripts/oracle/fetch-toolchain.sh "$work"
cargo build --release --example oracle_diff

for isa in esp32 esp32s2 esp32s3 esp8266; do
    python3 -I scripts/oracle/sweep.py "$toolchain" "$isa" --fixtures "tests/data/$isa.tsv" \
        | target/release/examples/oracle_diff "$isa" > "$work/$isa.result" \
        || { cat "$work/$isa.result"; exit 1; }
    cat "$work/$isa.result"
    # Exactly one well-formed digest, or the recorded one is left alone.
    digest=$(sed -n 's/^digest: //p' "$work/$isa.result")
    if ! [[ $digest =~ ^[0-9a-f]{16}$ ]]; then
        echo "$isa: no valid digest" >&2
        exit 1
    fi
    echo "$digest" > "tests/data/$isa.digest"
done

scripts/oracle/build_corpus.sh "$toolchain"
