#!/usr/bin/env python3
"""Samples objdump sweep output (from sweep.py) into a compact test fixture.

For every mnemonic, keeps the encodings with the smallest and largest word
(all free bits clear and all set) plus a few pseudo-random ones, and adds a
pseudo-random sample of encodings that objdump does not decode (including its
fallback decodes, see is_undecoded).

Random samples are "bottom-k": each encoding gets a key derived from its word
and the k smallest keys are kept. That makes samples of parts of a sweep
mergeable, so sweep.py --fixtures can sample chunks in parallel, and the
result depends only on the sweep's contents.

The fixture keeps objdump's text verbatim (minus symbol annotations) so the
tests apply the same normalization as the exhaustive comparison in
examples/oracle_diff.rs.

Usage: sweep.py <toolchain> <chip> | make_fixtures.py > tests/data/<chip>.tsv
  (or, sampling in parallel: sweep.py <toolchain> <chip> --fixtures <file>)
"""

import heapq
import sys

PER_MNEMONIC = 8
UNDECODED = 3000
MASK64 = (1 << 64) - 1


def is_undecoded(word, mnemonic):
    # When libisa cannot match an opcode it falls back to the first entry of
    # the configuration's opcode table: `lsi` on the ESP32 and `excw` on the
    # LX106. Those are only real if the word actually encodes them.
    return (
        mnemonic == ".byte"
        or (mnemonic == "lsi" and word & 0xF00F != 0x0003)
        or (mnemonic == "excw" and word != 0x002080)
    )


def key(word):
    """A pseudo-random sampling key for a word (splitmix64)."""
    z = (word + 0x9E3779B97F4A7C15) & MASK64
    z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK64
    z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK64
    return z ^ (z >> 31)


class BottomK:
    """The k entries with the smallest keys."""

    def __init__(self, k):
        self.k, self.heap = k, []  # max-heap of (-key, entry)

    def add(self, entry):
        item = (-key(entry[0]), entry)
        if len(self.heap) < self.k:
            heapq.heappush(self.heap, item)
        elif item > self.heap[0]:
            heapq.heapreplace(self.heap, item)

    def entries(self):
        return [entry for _, entry in self.heap]


class Sample:
    """A mergeable sample of (part of) a sweep."""

    def __init__(self):
        self.mnemonics = {}  # mnemonic -> [smallest, largest, BottomK]
        self.undecoded = BottomK(UNDECODED)

    def add(self, word, addr, length, text):
        """Adds one encoding; `text` is objdump's, with symbols stripped."""
        entry = (word, addr, length, text)
        mnemonic = text.split(" ", 1)[0]
        if is_undecoded(word, mnemonic):
            self.undecoded.add(entry)
            return
        state = self.mnemonics.get(mnemonic)
        if state is None:
            state = self.mnemonics[mnemonic] = [entry, entry, BottomK(PER_MNEMONIC)]
        else:
            state[0] = min(state[0], entry)
            state[1] = max(state[1], entry)
        state[2].add(entry)

    def export(self):
        """The sample as plain data, e.g. to send between processes."""
        mnemonics = {m: (lo, hi, s.entries()) for m, (lo, hi, s) in self.mnemonics.items()}
        return mnemonics, self.undecoded.entries()

    def merge(self, exported):
        """Merges in another sample, as returned by its export()."""
        mnemonics, undecoded = exported
        for entry in undecoded:
            self.undecoded.add(entry)
        for mnemonic, (smallest, largest, entries) in mnemonics.items():
            state = self.mnemonics.get(mnemonic)
            if state is None:
                state = self.mnemonics[mnemonic] = [smallest, largest, BottomK(PER_MNEMONIC)]
            else:
                state[0] = min(state[0], smallest)
                state[1] = max(state[1], largest)
            for entry in entries:
                state[2].add(entry)

    def render(self):
        picked = set(self.undecoded.entries())
        for smallest, largest, sample in self.mnemonics.values():
            picked.update([smallest, largest, *sample.entries()])
        return "".join("%06x\t%x\t%d\t%s\n" % entry for entry in sorted(picked))


def main():
    sample = Sample()
    for line in sys.stdin:
        word, addr, length, text = line.rstrip("\n").split("\t", 3)
        sample.add(int(word, 16), int(addr, 16), int(length), text.split(" <", 1)[0])
    sys.stdout.write(sample.render())


if __name__ == "__main__":
    main()
