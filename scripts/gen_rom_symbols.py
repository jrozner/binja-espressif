#!/usr/bin/env python3
"""Generates data/rom_symbols/<chip>.txt, the ROM symbol tables the plugin
embeds (see src/espressif/rom_symbols.rs for the format), from the ROM (and
ESP8266 peripheral) linker scripts.

The mask ROM of each chip is not part of a firmware image, but firmware calls
into it constantly (libc, libgcc, SPI flash and boot helpers). ESP-IDF (for
the ESP32 family) and the ESP8266 RTOS SDK publish the ROM's symbols as linker
scripts.

Peripheral register blocks are left to Espressif's SVD files
(https://github.com/espressif/svd) for the chips that have one, which Binary
Ninja's SVD loader applies with types and register fields. The ESP8266 has
no official SVD, so its peripheral blocks are named from its linker script.

Usage: scripts/gen_rom_symbols.py [--rom-elfs ELFDIR] DIR SOURCE CHIP...
  Regenerates data/rom_symbols/<chip>.txt for each CHIP; the other chips'
  files stay as they are. SOURCE describes where the linker scripts come
  from (e.g. "ESP-IDF v5.3") and is recorded in the file. DIR/<chip>/*.ld (or DIR/<chip>/ld/*.ld, so DIR can be
  esp-idf's components/esp_rom) are the files from esp-idf
  components/esp_rom/<chip>/ld/ (except *.api.ld, which are aliases),
  ESP8266_RTOS_SDK components/esp8266/ld/esp8266.{rom,peripherals}.ld and
  ESP8266_NONOS_SDK ld/eagle.rom.addr.v6.ld.

  ELFDIR holds Espressif's ROM ELF files (<chip>_rev<N>_rom.elf, from
  https://github.com/espressif/esp-rom-elfs releases); it is required for
  the RISC-V chips (their `code` ranges, below).

  The chips were generated from:
    esp8266                    ESP8266 RTOS and NONOS SDKs
    esp32, esp32s2, esp32s3    ESP-IDF v5.3
    esp32c2, esp32c3, esp32c6, ESP-IDF master (v6.2.0-dev, ddb3852ab5) and
    esp32h2, esp32c5           esp-rom-elfs 20260528
  so, for example, the RISC-V chips are regenerated with
    scripts/gen_rom_symbols.py --rom-elfs esp-rom-elfs \\
        esp-idf/components/esp_rom \\
        "ESP-IDF v6.2.0-dev (ddb3852ab5), esp-rom-elfs 20260528" \\
        esp32c2 esp32c3 esp32c6 esp32h2 esp32c5

The linker scripts do not say which symbols are functions. On the Xtensa
chips the ROM's code and data are in different address ranges, so the
loader tells them apart by region. On the RISC-V chips, ROM data shares
the ROM's address range with its code, so the files of those chips have
`code` lines: the address ranges of the executable sections of all its ROM
ELF files. Symbols in them are functions and all others data.
A symbol the ROM ELFs type as an object inside those ranges, or as a
function outside them, is an error.

Some scripts describe functions that only exist in the ROM of later chip
revisions. They go into separate per-revision tables (after a `revision`
line), so the loader can
apply them only to images that require that revision. Which file needs which
revision is taken from components/esp_rom/CMakeLists.txt (REVISIONS below);
an ECO number is not a revision number. Unknown revision-specific files are
an error.

The linker scripts also define constants that are not addresses (the
ESP32-S2's: the Xtensa HAL's release and configuration ids, the cache
attributes of its memory map) and labels of empty sections. Only symbols in
the chips' internal memory (INTERNAL_MEMORY below) are kept; the others are
listed as they are left out.

Within a chip's base or revision table, a name defined at two addresses is
an error, except for the two soft-float libraries in the RISC-V ROMs: rvfp
(linked by default) keeps the plain names and libgcc's conflicting ones get a
`_libgcc` suffix.
"""
import os
import re
import struct
import sys

PAT = re.compile(r"^\s*(?:PROVIDE\s*\(\s*)?([A-Za-z_.$][\w.$]*)\s*=\s*(0x[0-9a-fA-F]+)\s*\)?\s*;", re.M)

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "data", "rom_symbols")

# The supported chips.
CHIPS = ["esp8266", "esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c6", "esp32h2", "esp32c5"]

# The chips whose ROM code ranges come from their ROM ELF files.
ROM_CODE_CHIPS = {"esp32c2", "esp32c3", "esp32c6", "esp32h2", "esp32c5"}

# Revision-specific scripts, by the part of the file name between
# "<chip>.rom." and ".ld": (table suffix, minimum revision as major * 100 +
# minor). From the CONFIG_<CHIP>_REV_MIN_FULL conditions in esp_rom's
# CMakeLists.txt.
REVISIONS = {
    "esp32": {"eco3": ("ECO3", 300)},
    "esp32c2": {
        "eco4": ("ECO4", 200),
        "ble-eco4": ("ECO4", 200),
        # "For ESP32C2(ECO4), mbedTLS in ROM has been updated to v3.6.0-LTS"
        "mbedtls.eco4": ("ECO4", 200),
    },
    "esp32c3": {
        "eco3": ("ECO3", 3),
        "eco3_bt_funcs": ("ECO3", 3),
        "eco7": ("ECO7", 101),
        "eco7_bt_funcs": ("ECO7", 101),
    },
    "esp32c5": {"eco3": ("ECO3", 102)},
}

# Scripts left out, with the reason.
SKIP = {
    # Linked only below revision 2.0; the per-revision tables can only say
    # "this revision and later", and ble-eco4 replaces it from 2.0.
    "esp32c2": {"ble"},
}

# Where the ROM symbols of the ESP32 family can be: the chips' internal ROM
# and RAM, on the data bus (from the S3's and C3's DRAM up) and the
# instruction bus (up to the end of the C6's SRAM), and RTC (LP) memory. The
# ESP8266's tables also name its peripheral register blocks, and are kept
# whole.
INTERNAL_MEMORY = [(0x3FC8_0000, 0x4090_0000), (0x5000_0000, 0x5010_0000)]

HEADER = "# @generated by scripts/gen_rom_symbols.py; do not edit."


def parse(path):
    text = re.sub(r"/\*.*?\*/", "", open(path).read(), flags=re.S)
    for name, addr in PAT.findall(text):
        addr = int(addr, 16)
        if addr != 0 and not name.startswith("."):
            yield name, addr


def in_internal_memory(chip, addr):
    return chip == "esp8266" or any(lo <= addr < hi for lo, hi in INTERNAL_MEMORY)


def emit(out, syms):
    for name, addr in sorted(syms.items(), key=lambda kv: (kv[1], kv[0])):
        out.append(f"{addr:08x} {name}")


def read_elf(path):
    """The allocated sections (address, size, executable) and the function
    and object symbols ({address: {"func", "object"}}) of an ELF32 file."""
    data = open(path, "rb").read()
    if data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
        sys.exit(f"{path}: not a little-endian ELF32 file")
    shoff, = struct.unpack_from("<I", data, 0x20)
    shentsize, shnum = struct.unpack_from("<HH", data, 0x2E)
    headers = [struct.unpack_from("<IIIIIIIIII", data, shoff + i * shentsize) for i in range(shnum)]
    sections, symbols = [], {}
    for _, kind, flags, addr, offset, size, link, _, _, _ in headers:
        if flags & 2 and addr and size:  # SHF_ALLOC
            sections.append((addr, size, bool(flags & 4)))  # SHF_EXECINSTR
        if kind == 2:  # SHT_SYMTAB
            for off in range(offset, offset + size, 16):
                _, value, _, info, _, _ = struct.unpack_from("<IIIBBH", data, off)
                if info & 0xF in (1, 2):  # STT_OBJECT, STT_FUNC
                    symbols.setdefault(value, set()).add("func" if info & 0xF == 2 else "object")
    return sections, symbols


def rom_code(chip, elfdir):
    """The merged address ranges of the executable sections of the chip's ROM
    ELF files, and their symbols' types by address."""
    paths = sorted(
        os.path.join(elfdir, f) for f in os.listdir(elfdir) if re.fullmatch(rf"{chip}_rev\d+_rom\.elf", f)
    )
    if not paths:
        sys.exit(f"{chip}: no {chip}_rev<N>_rom.elf in {elfdir}")
    ranges, types = [], {}
    for path in paths:
        sections, symbols = read_elf(path)
        ranges += [(addr, addr + size) for addr, size, code in sections if code]
        for addr, kinds in symbols.items():
            types.setdefault(addr, set()).update(kinds)
    merged = []
    for start, end in sorted(ranges):
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))
    return merged, types, [os.path.basename(p) for p in paths]


def check_kinds(chip, syms, code, types):
    """Fails if the ROM ELFs contradict the code ranges for a symbol."""
    for name, addr in syms.items():
        in_code = any(start <= addr < end for start, end in code)
        kinds = types.get(addr, set())
        if in_code and kinds == {"object"} or not in_code and kinds == {"func"}:
            where = "inside" if in_code else "outside"
            sys.exit(f"{chip}: {name} at {addr:#x} is {' '.join(kinds)} {where} the ROM code")


def merge(chip, defs):
    """Turns {name: {addr: [file]}} into {name: addr}, resolving the rvfp/libgcc
    conflicts and failing on any other."""
    syms = {}
    for name, addrs in defs.items():
        if len(addrs) == 1:
            syms[name] = next(iter(addrs))
            continue
        by_lib = {}
        for addr, files in addrs.items():
            for f in files:
                by_lib[f.split(".rom.", 1)[1][:-3]] = addr
        if sorted(by_lib) == ["libgcc", "rvfp"] and len(addrs) == 2:
            syms[name] = by_lib["rvfp"]
            syms[name + "_libgcc"] = by_lib["libgcc"]
            continue
        detail = ", ".join(f"{a:#x} in {' '.join(fs)}" for a, fs in sorted(addrs.items()))
        sys.exit(f"{chip}: {name} is defined at several addresses: {detail}")
    for name in [n for n in syms if n.endswith("_libgcc")]:
        if name in defs:
            sys.exit(f"{chip}: {name} already exists")
    return syms


def generate(root, chip, source, elfdir):
    revisions = REVISIONS.get(chip, {})
    base, tables = {}, {}
    d = os.path.join(root, chip)
    if os.path.isdir(os.path.join(d, "ld")):  # esp-idf's components/esp_rom layout
        d = os.path.join(d, "ld")
    for f in sorted(os.listdir(d)):
        if not f.endswith(".ld") or f.endswith(".api.ld"):
            continue
        if f.endswith(".peripherals.ld") and chip != "esp8266":
            continue
        part = f.split(".rom.", 1)[1][:-3] if ".rom." in f else None
        if part in SKIP.get(chip, ()):
            continue
        if part in revisions:
            target = tables.setdefault(revisions[part], {})
        elif part is not None and "eco" in part:
            sys.exit(f"{chip}: no minimum revision is known for {f}; add it to REVISIONS")
        else:
            target = base
        for name, addr in parse(os.path.join(d, f)):
            if not in_internal_memory(chip, addr):
                print(f"{chip}: left out {name} = {addr:#x} ({f}), not in internal memory", file=sys.stderr)
                continue
            target.setdefault(name, {}).setdefault(addr, []).append(f)

    out = [HEADER, f"# {chip}: {source}"]
    base = merge(chip, base)
    tables = {key: merge(chip, defs) for key, defs in tables.items()}
    counts = ", ".join(f"{len(s)} from revision {rev}" for (_, rev), s in sorted(tables.items(), key=lambda kv: kv[0][1]))
    print(f"{chip}: {len(base)} symbols" + (f", {counts}" if counts else ""), file=sys.stderr)
    if chip in ROM_CODE_CHIPS:
        if elfdir is None:
            sys.exit(f"{chip}: --rom-elfs is required")
        code, types, files = rom_code(chip, elfdir)
        functions = 0
        for syms in [base, *tables.values()]:
            check_kinds(chip, syms, code, types)
            functions += sum(any(s <= a < e for s, e in code) for a in syms.values())
        total = len(base) + sum(len(s) for s in tables.values())
        print(f"{chip}: {functions} functions, {total - functions} data", file=sys.stderr)
        out.append(f"# code: the executable sections of {', '.join(files)}")
        out += [f"code {s:08x} {e:08x}" for s, e in code]
    emit(out, base)
    for (_, rev), syms in sorted(tables.items(), key=lambda kv: kv[0][1]):
        out.append(f"revision {rev}")
        emit(out, syms)
    return out


def main():
    args, elfdir = sys.argv[1:], None
    if args[:1] == ["--rom-elfs"] and len(args) > 1:
        elfdir, args = args[1], args[2:]
    if len(args) < 3:
        sys.exit(__doc__.split("\n\n")[3])
    root, source, chips = args[0], args[1], args[2:]
    for chip in chips:
        if chip not in CHIPS:
            sys.exit(f"unknown chip {chip}")
    os.makedirs(OUT, exist_ok=True)
    for chip in chips:
        lines = generate(root, chip, source, elfdir)
        with open(os.path.join(OUT, f"{chip}.txt"), "w", newline="\n") as f:
            f.write("\n".join(lines) + "\n")

main()
