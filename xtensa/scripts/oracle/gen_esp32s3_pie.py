#!/usr/bin/env python3
"""Generates src/pie/esp32s3.rs, the decode table for the ESP32-S3 Processor
Instruction Extensions (PIE): the `ee.*` vector instructions and the
`ld.qr`/`st.qr`/`mv.qr` moves, in both the 3-byte and the 4-byte format.

The table is derived from the toolchain's objdump, the same oracle the tests
use, and cross-checked against the encodings in chapter 1 of the ESP32-S3
Technical Reference Manual:

1. The fixed bits of each instruction come from disassembling every 3-byte
   word and a large random sample of 4-byte words.
2. Each operand is modeled as a linear function of the free bits: starting
   from the encoding with all free bits clear, flipping one bit at a time
   shows which operand it belongs to and its weight (negative for the sign
   bit of a signed field).
3. The model is checked against random encodings of every instruction before
   the table is written. scripts/oracle/run.sh then checks it exhaustively.

Usage: gen_esp32s3_pie.py <toolchain dir> > src/pie/esp32s3.rs
"""

import importlib.util
import multiprocessing
import os
import random
import re
import sys

spec = importlib.util.spec_from_file_location("sweep", os.path.join(os.path.dirname(__file__), "sweep.py"))
sweep = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sweep)

CHIP = "esp32s3"
BATCH = 1 << 16
SAMPLE_4B = 1 << 21
CHECKS = 64
REG = re.compile(r"^([afbmq])(\d+)$")
# 4-byte encodings without free bits, which random sampling cannot find: the
# format_32 `nop` (slot opcode 0x1cc20000 in the xtensa-overlays tables).
SINGLETONS_4B = [0xE601000E]


def is_pie(mnemonic):
    return mnemonic.startswith("ee.") or mnemonic in ("ld.qr", "st.qr", "mv.qr")


def disassemble(toolchain, words):
    # Workers import this script, not sweep.py, so call through it.
    return sweep.disassemble(toolchain, CHIP, words)


def run(toolchain, words):
    """Disassembles words in parallel batches."""
    batches = [words[i : i + BATCH] for i in range(0, len(words), BATCH)]
    with multiprocessing.Pool() as pool:
        results = pool.starmap(disassemble, [(toolchain, b) for b in batches])
    return [r for batch in results for r in batch]


def parse(text):
    mnemonic, _, rest = text.partition(" ")
    ops = []
    for op in rest.split(", ") if rest else []:
        m = REG.match(op)
        if m:
            ops.append((m.group(1), int(m.group(2))))
        else:
            v = int(op, 0)
            ops.append(("imm", v - (1 << 32) if v >= 1 << 31 else v))
    return mnemonic, ops


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    toolchain = os.path.abspath(sys.argv[1])
    rng = random.Random(0x53)

    # 1. Fixed bits per (mnemonic, length).
    words3 = [w for c in range(sweep.WIDE_CHUNKS) for w in sweep.candidates(c)]
    words4 = [((r >> 1) << 4) | 0xE | (r & 1) for r in (rng.getrandbits(29) for _ in range(SAMPLE_4B))]
    words4 += SINGLETONS_4B
    acc = {}
    for words in (words3, words4):
        for w, (length, text) in zip(words, run(toolchain, words)):
            mnemonic = text.split(" ", 1)[0]
            if is_pie(mnemonic) or (length == 4 and not text.startswith(".byte")):
                key = (mnemonic, length)
                a, o, n = acc.get(key, (0xFFFFFFFF, 0, 0))
                acc[key] = (a & w, o | w, n + 1)
    width_mask = {3: 0xFFFFFF, 4: 0xFFFFFFFF}
    insns = []
    for (mnemonic, length), (a, o, n) in sorted(acc.items()):
        fixed = ~(a ^ o) & width_mask[length]
        insns.append({"mnemonic": mnemonic, "len": length, "mask": fixed, "value": a & fixed})

    # 2. Operand model from single-bit flips.
    probes = []
    for insn in insns:
        free = [b for b in range(8 * insn["len"]) if not insn["mask"] >> b & 1]
        insn["free"] = free
        probes.append(insn["value"])
        probes.extend(insn["value"] | 1 << b for b in free)
    decoded = iter(run(toolchain, probes))
    for insn in insns:
        base_len, base_text = next(decoded)
        mnemonic, base = parse(base_text)
        assert mnemonic == insn["mnemonic"] and base_len == insn["len"], (insn, base_text)
        insn["operands"] = [{"kind": k, "base": v, "bits": []} for k, v in base]
        for b in insn["free"]:
            length, text = next(decoded)
            m, ops = parse(text)
            assert m == mnemonic and len(ops) == len(base), (mnemonic, b, text)
            changed = [i for i, (op, b0) in enumerate(zip(ops, base)) if op != b0]
            assert changed, (mnemonic, b, "bit changes no operand")
            for i in changed:
                assert ops[i][0] == base[i][0]
                insn["operands"][i]["bits"].append((b, ops[i][1] - base[i][1]))

    # 3. Check the model on random encodings.
    checks, expected = [], []
    for insn in insns:
        for _ in range(CHECKS):
            w = insn["value"]
            for b in insn["free"]:
                w |= rng.getrandbits(1) << b
            checks.append(w)
            expected.append(insn)
    failures = 0
    for w, insn, (length, text) in zip(checks, expected, run(toolchain, checks)):
        predicted = [
            (op["kind"], op["base"] + sum(d for b, d in op["bits"] if w >> b & 1)) for op in insn["operands"]
        ]
        if (length, parse(text)) != (insn["len"], (insn["mnemonic"], predicted)):
            failures += 1
            if failures < 20:
                print("model mismatch %08x: %s vs %s %s" % (w, text, insn["mnemonic"], predicted), file=sys.stderr)
    if failures:
        sys.exit("%d model mismatches" % failures)

    emit(insns)


def variant(mnemonic):
    return "".join(p[:1].upper() + p[1:] for p in mnemonic.replace("_", ".").split("."))


def segments(bits):
    """Groups (bit, weight) pairs into contiguous fields: (lsb, width, scale)."""
    segs = []
    for b, d in sorted(bits):
        if segs:
            lsb, width, scale = segs[-1]
            # The sign bit of a signed field has a negative weight and stays
            # a separate one-bit field.
            if b == lsb + width and d == scale << width:
                segs[-1] = (lsb, width + 1, scale)
                continue
        segs.append((b, 1, d))
    return segs


def hex32(v):
    return "0x%04x_%04x" % (v >> 16, v & 0xFFFF)


def emit(insns):
    out = sys.stdout
    out.write("// Generated by scripts/oracle/gen_esp32s3_pie.py; do not edit.\n")
    out.write("//\n// ESP32-S3 Processor Instruction Extensions: mask, value, length and\n")
    out.write("// operand fields of each instruction.\n\n")
    out.write("use super::{Field, Kind, Pie, Spec};\nuse crate::Opcode as O;\n\n")
    out.write("#[rustfmt::skip]\npub(super) static TABLE: &[Pie] = &[\n")
    for insn in insns:
        ops = []
        for op in insn["operands"]:
            fields = []
            for lsb, width, scale in segments(op["bits"]):
                fields.append("Field { lsb: %d, width: %d, scale: %d }" % (lsb, width, scale))
            kind = {"a": "Ar", "f": "Fr", "b": "Br", "m": "Mr", "q": "Qr", "imm": "Imm"}[op["kind"]]
            ops.append("Spec { kind: Kind::%s, base: %d, fields: &[%s] }" % (kind, op["base"], ", ".join(fields)))
        out.write("    // %s\n" % insn["mnemonic"])
        out.write(
            "    Pie { opcode: O::%s, len: %d, mask: %s, value: %s, operands: &[%s] },\n"
            % (variant(insn["mnemonic"]), insn["len"], hex32(insn["mask"]), hex32(insn["value"]), ", ".join(ops))
        )
    out.write("];\n")


if __name__ == "__main__":
    main()
