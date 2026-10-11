#!/usr/bin/env python3
"""Disassembles every possible Xtensa instruction encoding with GNU objdump.

Writes one line per encoding to stdout:

    <hex word>\t<hex address>\t<length>\t<objdump text>

Covered are every 24-bit word whose op0 selects a 3-byte instruction and every
16-bit word with any other op0. For chips with the 4-byte ESP32-S3 format
(op0 = 0xe or 0xf), every such 32-bit word is covered as well.

Each candidate is stored as its four little-endian bytes under its own
symbol, so objdump restarts decoding at every candidate and invalid encodings
cannot desynchronize the listing.

Usage: sweep.py <toolchain dir> <chip> [--fixtures FILE] > sweep.tsv
  e.g. sweep.py target/oracle/xtensa-esp-elf esp32

<chip> selects the toolchain's lib/xtensa_<chip>.so dynamic configuration.
With --fixtures, the workers also sample their chunks as make_fixtures.py
does, and the merged sample is written to FILE.
"""

import importlib.util
import multiprocessing
import os
import re
import subprocess
import sys
import tempfile

_spec = importlib.util.spec_from_file_location(
    "make_fixtures", os.path.join(os.path.dirname(os.path.abspath(__file__)), "make_fixtures.py")
)
make_fixtures = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(make_fixtures)

LINE = re.compile(r"^\s*([0-9a-f]+):\t([0-9a-f]+)\s*\t(.*)$")
CHUNK = 1 << 16
WIDE_CHUNKS = 256
NARROW_CHUNK = WIDE_CHUNKS
# Chips whose configuration has the 4-byte format.
FOUR_BYTE_CHIPS = {"esp32s3"}
FOUR_BYTE_CHUNKS = (1 << 29) // CHUNK


def candidates(chunk):
    """The words of a chunk; chunk numbering matches tests/common/digest.rs."""
    if chunk < WIDE_CHUNKS:
        return [v | chunk << 16 for v in range(CHUNK) if v & 0xF < 8]
    if chunk == NARROW_CHUNK:
        return [v for v in range(CHUNK) if v & 0xF >= 8]
    # 4-byte format: bits [3:1] are all set, the other 29 bits vary.
    base = (chunk - NARROW_CHUNK - 1) * CHUNK
    return [((i >> 1) << 4) | 0xE | (i & 1) for i in range(base, base + CHUNK)]


def disassemble(toolchain, chip, values):
    """Returns (length, text) for each word in values, as objdump decodes it."""
    prefix = os.path.join(toolchain, "bin", "xtensa-esp-elf-")
    env = dict(os.environ, XTENSA_GNU_CONFIG=os.path.join(toolchain, "lib", "xtensa_%s.so" % chip))
    with tempfile.TemporaryDirectory() as tmp:
        source, obj = os.path.join(tmp, "t.s"), os.path.join(tmp, "t.o")
        with open(source, "w") as f:
            f.write(".text\n")
            for i, v in enumerate(values):
                f.write("c%d: .byte %d,%d,%d,%d\n" % (i, v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFF, v >> 24))
        subprocess.run([prefix + "as", source, "-o", obj], check=True, env=env)
        listing = subprocess.run(
            [prefix + "objdump", "-d", "-z", obj], check=True, capture_output=True, text=True, env=env
        ).stdout
    decoded = {}
    for line in listing.splitlines():
        m = LINE.match(line)
        if m and int(m.group(1), 16) % 4 == 0:
            decoded[int(m.group(1), 16) // 4] = (
                len(m.group(2)) // 2,
                m.group(3).replace("\t", " ").strip(),
            )
    if len(decoded) != len(values):
        raise RuntimeError("decoded %d of %d" % (len(decoded), len(values)))
    return [decoded[i] for i in range(len(values))]


def sweep_chunk(args):
    """Returns a chunk's sweep output and, if requested, its fixture sample."""
    toolchain, chip, chunk, sample = args
    values = candidates(chunk)
    width = 8 if chunk > NARROW_CHUNK else 6
    decoded = disassemble(toolchain, chip, values)
    text = "".join(
        "%0*x\t%x\t%d\t%s\n" % (width, v, i * 4, length, line)
        for i, (v, (length, line)) in enumerate(zip(values, decoded))
    )
    if not sample:
        return text, None
    fixtures = make_fixtures.Sample()
    for i, (v, (length, line)) in enumerate(zip(values, decoded)):
        fixtures.add(v, i * 4, length, line.split(" <", 1)[0])
    # Plain data: workers cannot pickle classes from a module loaded by path.
    return text, fixtures.export()


def main():
    args = sys.argv[1:]
    fixtures_path = None
    if len(args) == 4 and args[2] == "--fixtures":
        fixtures_path = args[3]
        args = args[:2]
    if len(args) != 2:
        sys.exit(__doc__)
    toolchain, chip = os.path.abspath(args[0]), args[1]
    config = os.path.join(toolchain, "lib", "xtensa_%s.so" % chip)
    if not os.path.isfile(config):
        sys.exit("no configuration for %s: %s" % (chip, config))
    count = NARROW_CHUNK + 1 + (FOUR_BYTE_CHUNKS if chip in FOUR_BYTE_CHIPS else 0)
    chunks = [(toolchain, chip, c, fixtures_path is not None) for c in range(count)]
    fixtures = make_fixtures.Sample()
    with multiprocessing.Pool() as pool:
        for text, sample in pool.imap(sweep_chunk, chunks, chunksize=4):
            sys.stdout.write(text)
            if sample:
                fixtures.merge(sample)
    if fixtures_path:
        with open(fixtures_path, "w") as f:
            f.write(fixtures.render())


if __name__ == "__main__":
    main()
