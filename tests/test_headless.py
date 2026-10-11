#!/usr/bin/env python3
"""End-to-end tests of the plugin in headless Binary Ninja.

Loads the test ELFs (tests/data/*.elf, see scripts/build_test_elfs.sh) and
firmware images synthesized from them, and checks platform detection, the
calling conventions, call-site arguments, zero-overhead loop recovery and the
ESP image loader. Relocatable objects (tests/data/obj) are checked against
the relocations objdump lists for them.

Usage: tests/test_headless.py [path/to/libbinja_xtensa.so]

Runs Binary Ninja from ~/binaryninja (or $BN_INSTALL_DIR) with a temporary
user directory that contains only this plugin and a link to your license.
Everything the test writes goes in that directory, which is removed at exit
unless $BN_XTENSA_KEEP_TMP is set to a non-empty value.
"""
import atexit
import os
import shutil
import struct
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DATA = os.path.join(ROOT, "tests", "data")
plugin = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "release", "libbinja_xtensa.so"))
install = os.environ.get("BN_INSTALL_DIR", os.path.expanduser("~/binaryninja"))

user_dir = tempfile.mkdtemp(prefix="bn-xtensa-test-")


def remove_user_dir():
    if os.environ.get("BN_XTENSA_KEEP_TMP"):
        print(f"keeping {user_dir}")
    else:
        shutil.rmtree(user_dir, ignore_errors=True)


# Registered before importing binaryninja, which registers its shutdown with
# atexit too: handlers run last-in first-out, so this runs after the core has
# shut down. It runs on every normal exit: sys.exit, an uncaught exception
# (including KeyboardInterrupt), or the end of the script.
atexit.register(remove_user_dir)
os.makedirs(os.path.join(user_dir, "plugins"))
os.symlink(plugin, os.path.join(user_dir, "plugins", os.path.basename(plugin)))
license_file = os.path.expanduser("~/.binaryninja/license.dat")
if os.path.exists(license_file):
    os.symlink(license_file, os.path.join(user_dir, "license.dat"))
with open(os.path.join(user_dir, "settings.json"), "w") as f:
    f.write("{}")
os.environ["BN_USER_DIRECTORY"] = user_dir
sys.path.insert(0, os.path.join(install, "python"))

import binaryninja as bn  # noqa: E402

CHIPS = {"esp32": 0, "esp32s2": 2, "esp32s3": 9, "esp8266": None}
failures = []


def check(cond, what):
    print(("  ok   " if cond else "  FAIL ") + what)
    if not cond:
        failures.append(what)


def hlil_text(func):
    return "\n".join(str(line) for line in func.hlil.root.lines)


def func(bv, name):
    funcs = bv.get_functions_by_name(name)
    return funcs[0] if funcs else None


def check_program(bv, chip, label):
    check(bv.platform is not None and bv.platform.name == chip, f"{label}: platform {bv.platform}")
    windowed = chip != "esp8266"
    add3 = func(bv, "add3") or bv.get_function_at(symbols[chip]["add3"])
    check(add3 is not None and len(add3.parameter_vars) == 3, f"{label}: add3 has 3 parameters")
    cc = "windowed" if windowed else "call0"
    check(add3 is not None and add3.calling_convention.name == cc, f"{label}: add3 uses {cc}")
    start = bv.get_function_at(symbols[chip]["start"])
    text = hlil_text(start) if start else ""
    # Images have no symbols: fp and sink are data_<address> there.
    sink = "&sink" if label == "ELF" else f"&data_{symbols[chip]['sink']:x}"
    fp = "fp" if label == "ELF" else f"data_{symbols[chip]['fp']:x}"
    for call in ["(1, 2, 3)", "(1, 2, 3, 4, 5, 6, 7, 8)", f"{fp}(4, 5, 6)", f"({sink}, 1, 2)"]:
        check(call in text, f"{label}: start calls ...{call}")

    sum_ = bv.get_function_at(symbols[chip]["sum"])
    if chip in ("esp32", "esp32s3"):
        check(sum_ is not None and "lcount" in hlil_text(sum_), f"{label}: zero-overhead loop in sum")


def elf_symbols(path):
    """name -> address of the functions in an ELF32 file."""
    data = open(path, "rb").read()
    shoff, = struct.unpack_from("<I", data, 0x20)
    shentsize, shnum = struct.unpack_from("<HH", data, 0x2E)
    sections = [struct.unpack_from("<IIIIIIIIII", data, shoff + i * shentsize) for i in range(shnum)]
    out = {}
    for sh in sections:
        if sh[1] != 2:  # SHT_SYMTAB
            continue
        strtab = sections[sh[6]]
        for off in range(sh[4], sh[4] + sh[5], 16):
            name, value = struct.unpack_from("<II", data, off)
            end = data.index(b"\0", strtab[4] + name)
            out[data[strtab[4] + name:end].decode()] = value
    return out


def elf_to_image(path, chip_id, min_rev=0):
    """A minimal esptool-style image of an ELF's PT_LOAD segments."""
    data = open(path, "rb").read()
    entry, phoff = struct.unpack_from("<II", data, 0x18)
    phentsize, phnum = struct.unpack_from("<HH", data, 0x2A)
    segments = []
    for i in range(phnum):
        p_type, offset, vaddr, _, filesz = struct.unpack_from("<IIIII", data, phoff + i * phentsize)
        if p_type == 1 and filesz:
            segments.append((vaddr, data[offset:offset + filesz]))
    out = struct.pack("<BBBBI", 0xE9, len(segments), 2, 0x20, entry)
    if chip_id is not None:
        out += struct.pack("<B3sHBHH4sB", 0xEE, b"\0\0\0", chip_id, min_rev // 100, min_rev, 0xFFFF, b"\0" * 4, 0)
    for vaddr, body in segments:
        body += b"\0" * (-len(body) % 4)
        out += struct.pack("<II", vaddr, len(body)) + body
    out += b"\0" * (15 - len(out) % 16) + b"\xef"
    return out


def flash_dump(image, base):
    """A flash dump starting at flash offset `base` with the app at 0x10000."""
    dump = bytearray(b"\xff" * (0x10000 - base))
    entry = struct.pack("<2sBBII16sI", b"\xaa\x50", 0, 0, 0x10000, 0x100000, b"factory", 0)
    dump[0x8000 - base:0x8000 - base + 32] = entry
    return bytes(dump) + image


def load_bytes(blob, suffix):
    path = os.path.join(user_dir, f"image{len(os.listdir(user_dir))}{suffix}")
    with open(path, "wb") as f:
        f.write(blob)
    return bn.load(path)


symbols = {chip: elf_symbols(os.path.join(DATA, f"{chip}.elf")) for chip in CHIPS}

for chip, chip_id in CHIPS.items():
    print(f"{chip}:")
    elf = os.path.join(DATA, f"{chip}.elf")
    bv = bn.load(elf)
    check(bv.view_type == "ELF", "ELF view")
    check_program(bv, chip, "ELF")

    bv = load_bytes(elf_to_image(elf, chip_id), ".bin")
    check(bv.view_type == "ESP", f"image: view type {bv.view_type}")
    check_program(bv, chip, "image")
    check(bv.entry_point == struct.unpack_from("<I", open(elf, "rb").read(), 0x18)[0], "image: entry point")
    check(not bv.user_global_pointer_values and bv.get_symbol_by_raw_name("__global_pointer$") is None
          and bv.get_symbol_by_raw_name("_vector_table") is None,
          "image: no RISC-V global pointer or vector table")

    if chip_id is not None:
        for base in (0, 0x1000):
            bv = load_bytes(flash_dump(elf_to_image(elf, chip_id), base), ".bin")
            check(bv.view_type == "ESP" and bv.platform.name == chip, f"flash dump based at {base:#x}")

print("ESP8266 flash dumps:")
esp8266_image = elf_to_image(os.path.join(DATA, "esp8266.elf"), None)
bv = load_bytes(flash_dump(esp8266_image, 0), ".bin")
check(bv.view_type == "ESP" and "flash" not in bv.sections, "with a partition table, only the app partition is loaded")
nonos = bytearray(b"\xff" * 0x20000)
nonos[:len(esp8266_image)] = esp8266_image
bv = load_bytes(bytes(nonos), ".bin")
check("flash" in bv.sections, "without one (NONOS), the flash cache window is mapped")

print("Choosing the partition of a flash dump:")


def stub_image(entry):
    """A one-segment ESP32 image: `entry a1, 32; retw.n` at `entry`."""
    body = bytes.fromhex("3641001df0") + bytes(3)
    return struct.pack("<BBBBI", 0xE9, 1, 2, 0x20, entry) + bytes(16) + struct.pack("<II", entry, len(body)) + body


# Based at 0x1000 like `esptool merge_bin` output: bootloader at file offset 0.
esp32_elf = os.path.join(DATA, "esp32.elf")
dump = bytearray(flash_dump(elf_to_image(esp32_elf, 0), 0x1000))
dump[:0x40] = stub_image(0x40080400).ljust(0x40, b"\xff")
ota = struct.pack("<2sBBII16sI", b"\xaa\x50", 0, 0x10, 0x110000, 0x1000, b"ota_0", 0)
dump[0x8000 - 0x1000 + 32:0x8000 - 0x1000 + 64] = ota
dump = dump.ljust(0x110000 - 0x1000, b"\xff") + stub_image(0x40080800)
dump_path = os.path.join(user_dir, "partitions.bin")
with open(dump_path, "wb") as f:
    f.write(dump)
bv = bn.load(dump_path)
esp32_entry = struct.unpack_from("<I", open(esp32_elf, "rb").read(), 0x18)[0]
check(bv.view_type == "ESP" and bv.entry_point == esp32_entry, "default: the factory app")
settings = bn.BinaryViewType["ESP"].get_load_settings_for_data(bv.parent_view)
schema = settings.serialize_schema() if settings else ""
check(settings is not None and settings.contains("loader.platform")
      and '"enum":["","factory","ota_0","bootloader"]' in schema,
      "load settings offer the app partitions and the bootloader, next to the core's")
for choice, entry in [("factory", esp32_entry), ("ota_0", 0x40080800), ("bootloader", 0x40080400)]:
    bv = bn.load(dump_path, options={"loader.esp.partition": choice})
    check(bv.view_type == "ESP" and bv.entry_point == entry, f"loader.esp.partition = {choice}")
bv = bn.load(dump_path, options={"loader.esp.partition": "nvs"})
check(bv.view_type != "ESP", "a partition that is not an app is not loaded")
check(bn.BinaryViewType["ESP"].get_load_settings_for_data(load_bytes(stub_image(0x40080000), ".bin").parent_view)
      .contains("loader.esp.partition") is False, "no partition choice for a standalone image")

print("ESP32 revision-specific ROM symbols:")
elf = os.path.join(DATA, "esp32.elf")
for min_rev, expected in [(0, False), (300, True)]:
    bv = load_bytes(elf_to_image(elf, 0, min_rev), ".bin")
    sym = bv.get_symbol_at(0x4000F8D4)
    named = sym is not None and sym.name == "ets_use_secure_boot_v2"
    check(named == expected, f"min revision v{min_rev // 100}: ets_use_secure_boot_v2 {'named' if expected else 'not named'}")

print("ESP32-S3 RTC fast memory:")
# Code and retained data share RTC fast memory at 0x600FE000 (ESP-IDF
# aliases rtc_data_seg to rtc_iram_seg): initialized data there stays
# writable, and the rest is mapped.
s3_image = struct.pack("<BBBBI", 0xE9, 2, 2, 0, 0x40378000) + bytes([0xEE, 0, 0, 0]) + struct.pack("<H", 9) + bytes(10)
for addr, body in ((0x40378000, bytes.fromhex("3641001df0")), (0x600FE000, b"\x01\x00\x00\x00")):
    s3_image += struct.pack("<II", addr, len(body)) + body
bv = load_bytes(s3_image, ".bin")
data, gap = bv.get_segment_at(0x600FE000), bv.get_segment_at(0x600FE010)
check(bv.platform.name == "esp32s3" and data is not None and data.writable,
      "esp32s3: initialized RTC data at 0x600FE000 is writable")
check(gap is not None and gap.writable and not gap.executable,
      "esp32s3: uninitialized RTC memory at 0x600FE010 is mapped as data")

print("SVD files on the ESP view:")
esp32_image = elf_to_image(os.path.join(DATA, "esp32.elf"), 0)
bv = load_bytes(esp32_image, ".bin")
check({s.name for s in bv.get_symbols(0x3FF48000, 1)} == {"RTC_CNTL"}, "bundled SVD loaded automatically")
check(bv.get_data_var_at(0x3FF40000) is not None, "bundled SVD types peripherals")

svd = os.path.join(user_dir, "test.svd")
with open(svd, "w") as f:
    f.write("""<?xml version="1.0" encoding="utf-8"?>
<device schemaVersion="1.1"><name>ESP32</name><width>32</width><size>32</size><access>read-write</access>
<peripherals><peripheral><name>MY_RTC</name><baseAddress>0x3FF48000</baseAddress>
<addressBlock><offset>0x0</offset><size>0x100</size><usage>registers</usage></addressBlock>
<registers><register><name>OPTIONS0</name><addressOffset>0x0</addressOffset><size>32</size></register></registers>
</peripheral></peripherals></device>""")
image = os.path.join(user_dir, "svd-image.bin")
with open(image, "wb") as f:
    f.write(esp32_image)
bv = bn.load(image, options={"analysis.svd.autoLoadFile": svd})
check({s.name for s in bv.get_symbols(0x3FF48000, 1)} == {"MY_RTC"}, "an SVD the user chose wins")
check(not bv.get_symbols(0x3FF40000, 1), "the bundled SVD is not applied as well")

bv = bn.load(image, options={"espressif.svd.autoLoad": False})
check(not bv.get_symbols(0x3FF48000, 1), "espressif.svd.autoLoad = false leaves peripherals unnamed")

bv = load_bytes(elf_to_image(os.path.join(DATA, "esp8266.elf"), None), ".bin")
check({s.name for s in bv.get_symbols(0x60000000, 1)} == {"uart0"}, "ESP8266 peripherals are named (no official SVD)")

print("Chip platforms:")
for chip in CHIPS:
    platform = bn.Platform[chip]
    arch = bn.Architecture[f"xtensa-{chip}"]
    names = {cc.name for cc in platform.calling_conventions}
    expected = {"call0"} if chip == "esp8266" else {"call0", "windowed"}
    check(platform.arch == arch and names >= expected, f"{chip}: on {arch.name}, conventions {sorted(names)}")
    check(platform.default_calling_convention == arch.standalone_platform.default_calling_convention,
          f"{chip}: default convention {platform.default_calling_convention}")
RISCV_CHIPS = {"esp32c2": 12, "esp32c3": 5, "esp32c6": 13, "esp32h2": 16, "esp32c5": 23}
for chip in RISCV_CHIPS:
    platform = bn.Platform[chip]
    names = {cc.name for cc in platform.calling_conventions}
    check(platform.arch.name == "rv32gc" and names >= {"ilp32", "default"},
          f"{chip}: on {platform.arch.name}, conventions {sorted(names)}")
    check(platform.default_calling_convention.name == "ilp32", f"{chip}: default convention {platform.default_calling_convention}")
ilp32 = bn.Architecture["rv32gc"].calling_conventions["ilp32"]
check(ilp32.float_arg_regs == [] and ilp32.int_arg_regs == [f"a{n}" for n in range(8)]
      and ilp32.float_return_reg is None and ilp32.global_pointer_reg == "gp",
      "ilp32 passes everything in a0..a7")


def layout(cc, params, ret=None, permitted=None):
    """Each parameter's (or the return value's) location as a list of
    'a0'-style register names and 'sp+N' stack offsets, with '*' for a
    pointer to the value. `permitted` names the registers a call site sets."""
    def describe(loc):
        out = []
        for c in loc.components:
            v = c.var
            out.append(f"sp+{v.storage}" if v.source_type == bn.VariableSourceType.StackVariableSourceType
                       else cc.arch.get_reg_name(v.storage))
        return ("*" if getattr(loc, "indirect", False) else "") + ",".join(out)
    result = cc.get_call_layout(None, ret or bn.Type.void(),
                                [t if isinstance(t, bn.FunctionParameter) else bn.FunctionParameter(t, f"p{i}")
                                 for i, t in enumerate(params)],
                                permitted_regs=permitted and [cc.arch.get_reg_index(r) for r in permitted])
    return describe(result.return_value) if ret else [describe(p) for p in result.parameters]


# What Espressif GCC 15.2 does with -march=rv32imc -mabi=ilp32.
I32, F32, F64, U64 = bn.Type.int(4), bn.Type.float(4), bn.Type.float(8), bn.Type.int(8, False)
def struct_of(n):
    s = bn.StructureBuilder.create()
    for i in range(n):
        s.append(I32, f"f{i}")
    return bn.Type.structure_type(s)


PAIR, TRIPLE = struct_of(2), struct_of(3)
for label, params, expected in [
    ("float", [F32], ["a0"]),
    ("double", [F64], ["a0,a1"]),
    ("uint64_t", [U64], ["a0,a1"]),
    ("int, double", [I32, F64], ["a0", "a1,a2"]),
    ("int, float, int", [I32, F32, I32], ["a0", "a1", "a2"]),
    ("7 ints, double", [I32] * 7 + [F64], [f"a{n}" for n in range(7)] + ["a7,sp+0"]),
    ("5 doubles", [F64] * 5, ["a0,a1", "a2,a3", "a4,a5", "a6,a7", "sp+0"]),
    ("9 ints, double", [I32] * 9 + [F64], [f"a{n}" for n in range(8)] + ["sp+0", "sp+8"]),
    ("8-byte struct", [PAIR], ["a0,a1"]),
    ("12-byte struct", [TRIPLE], ["*a0"]),
]:
    got = layout(ilp32, params)
    check(got == expected, f"ilp32 f({label}): {got}")
for label, ret, expected in [("float", F32, "a0"), ("double", F64, "a0,a1"), ("uint64_t", U64, "a0,a1")]:
    got = layout(ilp32, [], ret)
    check(got == expected, f"ilp32 returns {label} in {got}")

# What Espressif GCC 15.2 does on the ESP32 (xtensa-esp-elf): a value
# aligned to 8 bytes starts at an even argument register, aggregates go in
# registers, and an argument that does not fit in the registers left goes on
# the stack, with all those after it.
for cc_name, regs in (("windowed", [f"a{n}" for n in range(10, 16)]), ("call0", [f"a{n}" for n in range(2, 8)])):
    cc = bn.Architecture["xtensa-esp32"].calling_conventions[cc_name]
    r = regs
    for label, params, expected in [
        ("uint64_t", [U64], [f"{r[0]},{r[1]}"]),
        ("int, uint64_t, int", [I32, U64, I32], [r[0], f"{r[2]},{r[3]}", r[4]]),
        ("5 ints, double, int", [I32] * 5 + [F64, I32], r[:5] + ["sp+0", "sp+8"]),
        ("7 ints, double", [I32] * 7 + [F64], r + ["sp+0", "sp+8"]),
        ("12-byte struct", [TRIPLE], [",".join(r[:3])]),
        ("int, 12-byte struct, int, int", [I32, TRIPLE, I32, I32], [r[0], ",".join(r[1:4]), r[4], r[5]]),
    ]:
        got = layout(cc, params)
        check(got == expected, f"{cc_name} f({label}): {got}")
    got = layout(cc, [], U64)
    check(got == f"{r[0]},{r[1]}", f"{cc_name} returns uint64_t in {got}")
    # A call site that sets only the registers it uses leaves the register
    # an 8-byte argument skips unset.
    got = layout(cc, [I32, U64], permitted=[r[0], r[2], r[3]])
    check(got == [r[0], f"{r[2]},{r[3]}"], f"{cc_name} f(int, uint64_t) at a call site setting {r[0]}, "
                                           f"{r[2]}, {r[3]}: {got}")
    got = layout(cc, [bn.FunctionParameter(TRIPLE, "p", source=bn.ValueLocationSource.PassByReferenceLocationSource),
                      I32])
    check(got == [f"*{r[0]}", r[1]], f"{cc_name} f(12-byte struct passed by reference, int): {got}")


# Parameters with a location the user gave them: the ones after them skip
# its registers and stack. Binary Ninja's own `default` convention does the
# same where the fixed register is the next one, but otherwise reuses it
# (a3 fixed, 4 ints: a0, a1, a2, a3), overlapping the fixed parameter.
def fixed_at(loc, ty=I32):
    var = (bn.variable.CoreVariable(bn.VariableSourceType.StackVariableSourceType, 0, loc) if isinstance(loc, int)
           else bn.variable.CoreVariable(bn.VariableSourceType.RegisterVariableSourceType, 0,
                                         ilp32.arch.get_reg_index(loc)))
    return bn.FunctionParameter(ty, "fixed", var)


default_cc = ilp32.arch.calling_conventions["default"]
for label, params, expected, as_default in [
    ("a0 fixed, int", [fixed_at("a0"), I32], ["a0", "a1"], True),
    ("a3 fixed, 4 ints", [fixed_at("a3")] + [I32] * 4, ["a3", "a0", "a1", "a2", "a4"], False),
    ("int, a0 fixed, int", [I32, fixed_at("a0"), I32], ["a0", "a0", "a1"], True),
    ("sp+4 fixed, 9 ints", [fixed_at(4)] + [I32] * 9, ["sp+4"] + [f"a{n}" for n in range(8)] + ["sp+8"], True),
]:
    got = layout(ilp32, params)
    check(got == expected and (got == layout(default_cc, params)) == as_default,
          f"ilp32 f({label}): {got} (default: {layout(default_cc, params)})")
check(layout(ilp32, [fixed_at("a1"), F64]) == ["a1", "a0,a2"], "ilp32: a double skips a fixed register")
# A 2-byte value fixed at sp+6 ends at sp+8; a double split after seven ints
# has its high word there, word-aligned.
got = layout(ilp32, [fixed_at(6, bn.Type.int(2))] + [I32] * 7 + [F64])
check(got[-1] == "a7,sp+8", f"ilp32: the stack after an unaligned fixed slot is word-aligned ({got})")
check({p.name for p in bn.Platform.get_list(os="espressif")} == set(CHIPS) | set(RISCV_CHIPS),
      "registered under the espressif OS")
for name in ("esp32s3", "xtensa-esp32s3"):
    bv = bn.load(os.path.join(DATA, "esp32s2.elf"), options={"loader.platform": name})
    check(bv.platform.name == name, f"loader.platform = {name} overrides the ELF's chip")

print("Lifting semantics:")
esp32 = bn.Architecture["xtensa-esp32"]


def llil(arch, code, addr=0x40080000):
    return arch.get_low_level_il_from_bytes(bytes.fromhex(code), addr)


for code, name, op in [("4023a2", "muluh", "LLIL_MULU_DP"), ("4023b2", "mulsh", "LLIL_MULS_DP")]:
    shift = llil(esp32, code).src.src
    wide = shift.left
    check(wide.operation.name == op and wide.size == 4 and wide.left.size == 4 and shift.size == 8,
          f"{name}: 4-byte operands, 8-byte product")
for code, name, op in [("0023a1", "sll", "LLIL_LSR"), ("302091", "srl", "LLIL_LSR"),
                       ("3020b1", "sra", "LLIL_ASR")]:
    il = llil(esp32, code)
    check(il.src.operation.name == "LLIL_LOW_PART" and il.src.src.operation.name == op
          and il.src.src.size == 8, f"{name}: 64-bit funnel shift (exact for SAR 0 and 32)")
for code, name in [("20014a", "madd.s"), ("20016a", "maddn.s"), ("20015a", "msub.s")]:
    il = llil(esp32, code)
    check(il.src.operation.name == "LLIL_FLOAT_CONV" and il.src.src.size == 8,
          f"{name}: accumulates in double precision (one rounding of the product)")

print("Zero-overhead loops are per architecture:")
esp32.get_instruction_info(bytes.fromhex("768020"), 0x40080000)  # loop a0, 0x40080024
nop = bytes.fromhex("f02000")
check(len(esp32.get_instruction_info(nop, 0x40080021).branches) == 2, "ESP32: loop end branches back")
check(not bn.Architecture["xtensa-esp8266"].get_instruction_info(nop, 0x40080021).branches,
      "ESP8266: no loop edge from an ESP32 loop at the same address")

print("RISC-V images:")


def riscv_image(chip_id, segments, entry):
    out = struct.pack("<BBBBI", 0xE9, len(segments), 2, 0, entry)
    out += bytes([0xEE, 0, 0, 0]) + struct.pack("<H", chip_id) + bytes(10)
    for addr, body in segments:
        out += struct.pack("<II", addr, len(body)) + body
    return out


def section_at(bv, addr):
    sections = bv.get_sections_at(addr)
    return sections[0] if sections else None


# addi sp, sp, -16; sw ra, 12(sp); li a0, 1; lw ra, 12(sp); addi sp, sp, 16; ret
rv_code = bytes.fromhex("130101ff" "06c6" "0545" "b240" "4101" "8280")
app_desc = struct.pack("<I", 0xABCD5432) + bytes(252)
layouts = {
    # chip: (code address, rodata address, a .bss address, ROM data in SRAM)
    "esp32c3": (0x40380000, 0x3C000020, 0x3FC90000, 0x3FCDF800),
    "esp32c6": (0x40800000, 0x42000020, 0x40810000, 0x4087FF00),
}
for chip, (code_addr, rodata_addr, bss_addr, rom_data_addr) in layouts.items():
    blob = riscv_image(RISCV_CHIPS[chip], [(rodata_addr, app_desc), (code_addr, rv_code)], code_addr)
    bv = load_bytes(blob, ".bin")
    bv.update_analysis_and_wait()
    check(bv.view_type == "ESP" and bv.platform.name == chip and bv.arch.name == "rv32gc",
          f"{chip}: ESP view on {bv.platform.name if bv.platform else None}")
    sym = bv.get_symbol_at(code_addr)
    check(sym is not None and sym.name == "call_start_cpu0", f"{chip}: entry symbol")
    f = bv.get_function_at(code_addr)
    check(f is not None and f.calling_convention.name == "ilp32", f"{chip}: entry function uses ilp32")
    check(f is not None and "return 1" in hlil_text(f), f"{chip}: entry function decompiles")
    check(bv.get_symbol_by_raw_name("esp_app_desc") is not None and bv.get_symbol_by_raw_name("esp_app_desc").address == rodata_addr,
          f"{chip}: esp_app_desc in the rodata segment")
    code_seg, rodata_seg = bv.get_segment_at(code_addr), bv.get_segment_at(rodata_addr)
    check(code_seg is not None and code_seg.executable, f"{chip}: code segment is executable")
    check(rodata_seg is not None and not rodata_seg.writable, f"{chip}: rodata segment is read-only")
    bss = bv.get_segment_at(bss_addr)
    check(bss is not None and bss.writable and not bss.executable, f"{chip}: .bss gap mapped as data")
    check(bv.get_segment_at(rom_data_addr) is not None, f"{chip}: the ROM's data in SRAM is mapped")
    check({s.name for s in bv.get_symbols(0x60000000, 1)} == {"UART0"}, f"{chip}: bundled SVD loaded automatically")
    check(not bv.user_global_pointer_values, f"{chip}: no gp without ESP-IDF's startup code")
    if chip == "esp32c6":
        check(code_seg.writable, "esp32c6: unified SRAM is writable (code and data share it)")
        sec = section_at(bv, rodata_addr)
        check(sec is not None and sec.semantics == bn.SectionSemantics.ReadOnlyCodeSectionSemantics,
              "esp32c6: the shared flash window is read-only code")
    else:
        check(not code_seg.writable, "esp32c3: IRAM is mapped read-only")
        sec = section_at(bv, rodata_addr)
        check(sec is not None and sec.semantics == bn.SectionSemantics.ReadOnlyDataSectionSemantics,
              "esp32c3: DROM is read-only data")
bv = load_bytes(riscv_image(23, [(0x40800000, rv_code)], 0x40800000), ".bin")
check(bv.view_type == "ESP" and bv.platform.name == "esp32c5", "esp32c5: ESP view (no SVD)")
bv = load_bytes(riscv_image(18, [(0x4FF00000, rv_code)], 0x4FF00000), ".bin")
check(bv.view_type != "ESP", "ESP32-P4 images are not loaded")

print("RISC-V ROM symbols:")
# On the C6, H2 and C5 the ROM's data shares its address range with its code.
rom_kinds = {
    "esp32c6": {"memcpy": "Function", "he_max_apep_length": "Data", "crc8_be_table_ptr": "Data",
                "g_coex_param_ptr": "Data"},
    "esp32h2": {"memcpy": "Function", "ets_printf": "Function", "crc8_be_table_ptr": "Data"},
    "esp32c3": {"memcpy": "Function", "g_coex_param_ptr": "Data"},
}
code_addrs = {"esp32c6": 0x40800000, "esp32h2": 0x40800000, "esp32c3": 0x40380000}
for chip, kinds in rom_kinds.items():
    bv = load_bytes(riscv_image(RISCV_CHIPS[chip], [(code_addrs[chip], rv_code)], code_addrs[chip]), ".bin")
    for name, kind in kinds.items():
        sym = bv.get_symbol_by_raw_name(name)
        expected = bn.SymbolType.ImportedFunctionSymbol if kind == "Function" else bn.SymbolType.DataSymbol
        check(sym is not None and sym.type == expected,
              f"{chip}: ROM {name} is {kind.lower()} ({sym.type.name if sym else 'missing'})")
# RTC (C3) and LP (C6) memory holds code and retained data at one address:
# initialized data there stays writable, and the rest (`.rtc.bss`,
# `.rtc_noinit`) is mapped.
for chip, code_addr in (("esp32c3", 0x40380000), ("esp32c6", 0x40800000)):
    bv = load_bytes(riscv_image(RISCV_CHIPS[chip], [(code_addr, rv_code), (0x50000000, b"\x01\x00\x00\x00")],
                                code_addr), ".bin")
    data, gap = bv.get_segment_at(0x50000000), bv.get_segment_at(0x50000010)
    check(data is not None and data.writable, f"{chip}: initialized RTC data at 0x50000000 is writable")
    check(gap is not None and gap.writable and not gap.executable,
          f"{chip}: uninitialized RTC memory at 0x50000010 is mapped as data")
# A pre-ESP-IDF 5.0 C3 image (no min_chip_rev_full) requiring v0.3: the
# legacy byte is the C3's minor revision. The v0.3 (ECO3) ROM symbols apply;
# the v1.1 (ECO7) ones do not.
legacy_c3 = bytearray(riscv_image(5, [(0x40380000, rv_code)], 0x40380000))
legacy_c3[8 + 6] = 3
bv = load_bytes(bytes(legacy_c3), ".bin")
sym = bv.get_symbol_by_raw_name("wDevCheckBlockError")
check(sym is not None and sym.address == 0x400017B4, "legacy C3 v0.3 image: ECO3 ROM symbols")
check(bv.get_symbol_by_raw_name("r_bt_bb_isr_hack") is None, "legacy C3 v0.3 image: no ECO7 ROM symbols")

print("RISC-V images built by GCC:")
# tests/data/esp32c{3,6}.elf: ESP-IDF-style startup (`la gp,
# __global_pointer$`, mtvec at a 32-entry `_vector_table`) and the corpus.
for chip in ("esp32c3", "esp32c6"):
    elf = os.path.join(DATA, f"{chip}.elf")
    syms = elf_symbols(elf)
    bv = load_bytes(elf_to_image(elf, RISCV_CHIPS[chip]), ".bin")
    gp = syms["__global_pointer$"]
    values = bv.user_global_pointer_values
    check(len(values) == 1 and values[0][0] == "gp" and values[0][1].value == gp,
          f"{chip}: gp {gp:#x} recovered from call_start_cpu0 ({values})")
    sym = bv.get_symbol_by_raw_name("__global_pointer$")
    check(sym is not None and sym.address == gp, f"{chip}: __global_pointer$ defined")
    start = bv.get_function_at(syms["start"])
    text = hlil_text(start) if start else ""
    check(f"&data_{syms['sink']:x}, 1, 2)" in text and f"data_{syms['fp']:x}(4, 5, 6)" in text,
          f"{chip}: gp-relative accesses in start resolve to sink and fp")
    check("gp" not in text, f"{chip}: start does not take gp as an input")
    for name in ("_vector_table", "_panic_handler", "_interrupt_handler"):
        f = bv.get_function_at(syms[name])
        check(f is not None and f.name == name, f"{chip}: {name} found and named")
    entry = bv.get_function_at(bv.entry_point)
    text = hlil_text(entry) if entry else ""
    check(f"_csrwr(0x305, {syms['_vector_table'] | 1:#x})" in text and f"sub_{syms['start']:x}()" in text,
          f"{chip}: call_start_cpu0 sets mtvec and calls start")

# User global pointer values saved in a database (here a user gp value and
# one for tp) are what a reopened view uses. (A reopened view does not set
# gp, so adding gp next to other registers' values, which the view does when
# it finds them, is covered by a unit test instead.)
elf = os.path.join(DATA, "esp32c3.elf")
bv = load_bytes(elf_to_image(elf, RISCV_CHIPS["esp32c3"]), ".bin")
bv.set_user_global_pointer_values([("tp", bn.variable.ConstantPointerRegisterValue(0x1000)),
                                   ("gp", bn.variable.ConstantPointerRegisterValue(0x1234))])
db = os.path.join(user_dir, "gp_tp.bndb")
bv.create_database(db)
bv.file.close()
bv = bn.load(db)
values = {reg: v.value for reg, v in bv.user_global_pointer_values}
check(values == {"tp": 0x1000, "gp": 0x1234}, f"saved user global pointer values are kept ({values})")
bv.file.close()


def check_gp_set_once(path, gp, label):
    """gp is set when `path` is first loaded, but not again once the user
    has changed or cleared it, when the database is reopened (the core does
    not save an empty list of user global pointer values, nor tell it from
    values never set)."""
    for user_values, what in (([], "cleared"), ([("gp", bn.variable.ConstantPointerRegisterValue(0x1234))], "changed")):
        bv = bn.load(path)
        values = bv.user_global_pointer_values
        check(len(values) == 1 and values[0][1].value == gp, f"{label}: gp {gp:#x} set on first load")
        bv.set_user_global_pointer_values(user_values)
        db = os.path.join(user_dir, f"gp_{what}_{os.path.basename(path)}.bndb")
        bv.create_database(db)
        bv.file.close()
        for reopen in ("reopened", "reopened again"):
            bv = bn.load(db)
            bv.update_analysis_and_wait()
            values = [(reg, v.value) for reg, v in bv.user_global_pointer_values]
            check(values == [(r, v.value) for r, v in user_values],
                  f"{label}: gp the user {what} stays {what} when {reopen} ({values})")
            bv.file.save_auto_snapshot()
            bv.file.close()


image = os.path.join(user_dir, "gp_cleared.bin")
with open(image, "wb") as f:
    f.write(elf_to_image(elf, RISCV_CHIPS["esp32c3"]))
check_gp_set_once(image, elf_symbols(elf)["__global_pointer$"], "esp32c3 image")

print("RISC-V ELF files:")


def riscv_elf(vaddr, flags=0x1, abs_symbols=(), memsz=None):
    """A minimal executable RV32 ELF: `li a0, 1; ret` loaded at `vaddr`,
    with `e_flags` = `flags` (0x1 = RVC, soft-float) and a symbol table of
    `ABS` symbols. `memsz` makes the segment that much bigger in memory."""
    code = struct.pack("<II", 0x00100513, 0x00008067)
    strtab = b"\0"
    syms = bytes(16)
    for name, addr in abs_symbols:
        syms += struct.pack("<IIIBBH", len(strtab), addr, 0, 0x12, 0, 0xFFF1)
        strtab += name.encode() + b"\0"
    shstrtab = b"\0.text\0.symtab\0.strtab\0.shstrtab\0"
    text_off = 0x100
    symtab_off = text_off + len(code)
    strtab_off = symtab_off + len(syms)
    shstrtab_off = strtab_off + len(strtab)
    shoff = (shstrtab_off + len(shstrtab) + 3) & ~3
    header = b"\x7fELF\x01\x01\x01" + bytes(9) + struct.pack(
        "<HHIIIIIHHHHHH", 2, 243, 1, vaddr, 52, shoff, flags, 52, 32, 1, 40, 5, 4)
    phdr = struct.pack("<IIIIIIII", 1, text_off, vaddr, vaddr, len(code), memsz or len(code), 5, 4)
    sections = bytes(40) + b"".join(struct.pack("<IIIIIIIIII", *s) for s in [
        (1, 1, 6, vaddr, text_off, len(code), 0, 0, 4, 0),
        (7, 2, 0, 0, symtab_off, len(syms), 3, 1, 4, 16),
        (15, 3, 0, 0, strtab_off, len(strtab), 0, 0, 1, 0),
        (23, 3, 0, 0, shstrtab_off, len(shstrtab), 0, 0, 1, 0),
    ])
    out = header + phdr
    out += bytes(text_off - len(out)) + code + syms + strtab + shstrtab
    return out + bytes(shoff - len(out)) + sections


for chip in ("esp32c3", "esp32c6"):
    bv = bn.load(os.path.join(DATA, f"{chip}.elf"))
    check(bv.view_type == "ELF" and bv.platform.name == chip, f"{chip}.elf: ELF view on {bv.platform}")
    check(bv.platform.default_calling_convention.name == "ilp32", f"{chip}.elf: ilp32 by default")
# Not Espressif firmware: Binary Ninja's own platform is kept.
for label, blob in [("loaded at 0x10000", riscv_elf(0x10000)),
                    ("hard-float", riscv_elf(0x42000020, flags=0x3)),
                    ("partly outside every chip's memory", riscv_elf(0x4087FFFC)),
                    # Both ends in the C3's memory map (its ROM data alias
                    # and its ROM), with 896 KiB of unmapped memory between.
                    ("spanning unmapped memory", riscv_elf(0x3FF1FFF8, memsz=0x40000008 - 0x3FF1FFF8))]:
    bv = load_bytes(blob, ".elf")
    check(bv.view_type == "ELF" and bv.arch.name == "rv32gc" and bv.platform.name not in RISCV_CHIPS,
          f"a RISC-V ELF {label} keeps {bv.platform}")
bv = load_bytes(riscv_elf(0x42000020), ".elf")
check(bv.platform.name == "esp32c3", f"a soft-float ELF in the C3's flash is ESP32-C3 code ({bv.platform})")
# The C6, H2 and C5 share these addresses; the ROM symbols the file uses
# decide. memcpy and memset are at different addresses in each ROM;
# ets_printf is at the same address in the C6's and the H2's.
h2_rom = [("memcpy", 0x400004A4), ("memset", 0x400004A0), ("ets_printf", 0x40000028)]
bv = load_bytes(riscv_elf(0x40800000), ".elf")
check(bv.platform.name == "esp32c6", f"unified SRAM without ROM symbols: ESP32-C6 ({bv.platform})")
bv = load_bytes(riscv_elf(0x40800000, abs_symbols=h2_rom), ".elf")
check(bv.platform.name == "esp32h2", f"H2 ROM symbols pick the ESP32-H2 ({bv.platform})")
bv = bn.load(os.path.join(DATA, "esp32c3.elf"), options={"loader.platform": "esp32c2"})
check(bv.platform.name == "esp32c2", "loader.platform overrides a RISC-V ELF's chip")
path = os.path.join(user_dir, "h2_rom.elf")
with open(path, "wb") as f:
    f.write(riscv_elf(0x40800000, abs_symbols=h2_rom))
bv = bn.load(path, options={"loader.platform": "esp32c5"})
check(bv.platform.name == "esp32c5", "loader.platform overrides ROM symbols")

print("ESP-IDF ELF fix-ups:")
# tests/data/idf_*.elf: laid out like ESP-IDF apps, with a writable
# `.flash.rodata` (WA, in an RW segment) holding a 12-entry jump table, and
# the mask ROM's symbols as `ABS` symbols.
IDF_ELFS = {
    "esp32": dict(jump=0x400d0086, peripheral=(0x3FF48000, "RTC_CNTL"),
                  functions=["ets_printf", "memcpy", "memset"], data=["_global_impure_ptr", "_ctype_"]),
    "esp32s3": dict(jump=0x42000086, peripheral=(0x60000000, "UART0"),
                    functions=["ets_printf", "memcpy", "memset"], data=["_global_impure_ptr", "g_coex_param_ptr"]),
    "esp32c3": dict(jump=0x42000058, peripheral=(0x60000000, "UART0"),
                    functions=["ets_printf", "memset", "memcpy"], data=["g_coex_param_ptr"]),
    "esp32c6": dict(jump=0x42000058, peripheral=(0x60000000, "UART0"),
                    functions=["memcpy"], data=["crc8_be_table_ptr"]),
}


def check_idf_elf(bv, chip, case, syms, label):
    rodata = bv.get_section_by_name(".flash.rodata")
    check(rodata is not None and rodata.semantics == bn.SectionSemantics.ReadOnlyDataSectionSemantics,
          f"{label}: .flash.rodata is read-only data")
    targets = {b.dest_addr for f in bv.get_functions_containing(case["jump"])
               for b in f.indirect_branches if b.source_addr == case["jump"]}
    check(len(targets) == 12, f"{label}: the jump table at {case['jump']:#x} resolves ({len(targets)} targets)")
    for kinds, expected in ((case["functions"], bn.SymbolType.ImportedFunctionSymbol),
                            (case["data"], bn.SymbolType.DataSymbol)):
        for name in kinds:
            sym = bv.get_symbol_by_raw_name(name)
            check(sym is not None and sym.address == syms[name] and sym.type == expected,
                  f"{label}: ROM {name} is a {expected.name} at {syms[name]:#x}")
    if chip in RISCV_CHIPS:
        gp = syms["__global_pointer$"]
        values = bv.user_global_pointer_values
        check(len(values) == 1 and values[0][1].value == gp, f"{label}: gp {gp:#x} from __global_pointer$")
    addr, name = case["peripheral"]
    check(name in {s.name for s in bv.get_symbols(addr, 1)}, f"{label}: bundled SVD names {name}")


for chip, case in IDF_ELFS.items():
    elf = os.path.join(DATA, f"idf_{chip}.elf")
    syms = elf_symbols(elf)
    bv = bn.load(elf)
    check(bv.view_type == "ELF" and bv.platform.name == chip, f"idf_{chip}.elf: ELF on {chip}")
    check_idf_elf(bv, chip, case, syms, f"idf_{chip}.elf")
    if chip == "esp32":
        # Not in the ELF: from the bundled table, every revision's only.
        sym = bv.get_symbol_by_raw_name("ets_delay_us")
        check(sym is not None and sym.address == 0x40008534, "idf_esp32.elf: bundled ROM table fills in ets_delay_us")
        check(bv.get_symbol_by_raw_name("ets_use_secure_boot_v2") is None,
              "idf_esp32.elf: revision-specific ROM symbols are not guessed")
    sections = len(bv.sections)
    db = os.path.join(user_dir, f"idf_{chip}.bndb")
    check(bv.create_database(db), f"idf_{chip}.elf: saved")
    bv.file.close()
    bv = bn.load(db)
    check_idf_elf(bv, chip, case, syms, f"idf_{chip}.bndb")
    check(len(bv.sections) == sections, f"idf_{chip}.bndb: no duplicate sections")
    bv.file.close()

elf = os.path.join(DATA, "idf_esp32c3.elf")
check_gp_set_once(elf, elf_symbols(elf)["__global_pointer$"], "idf_esp32c3.elf")

print("Mask ROM types:")
# tests/data/idf_*.elf's rom_calls calls ROM functions of one and three
# arguments and the ROM's libgcc helpers, and reads the ROM's flash chip
# structure (g_rom_flashchip on the ESP32, rom_spiflash_legacy_data->chip on
# the others). With the ROM's types, the calls have the prototypes'
# arguments and the read names the field.
ROM_CALLS = {"ets_delay_us": 1, "memset": 3, "__floatsidf": 1, "__muldf3": 2, "__fixdfsi": 1, "__udivdi3": 2}
ROM_TYPED_CHIPS = {"esp32": 0, "esp32s3": 9, "esp32c3": 5, "esp32c6": 13}


def rom_regions(bv):
    return [r for r in bv.memory_map.regions if r.name.startswith("espressif.rom")]


def rom_functions(bv):
    return [f for f in bv.functions if bv.memory_map.get_active_memory_region_at(f.start).startswith("espressif.rom")]


def check_rom_types(bv, chip, syms, label):
    """The ROM calls in rom_calls (at its address in `syms`) and their
    callees."""
    f = bv.get_function_at(syms["rom_calls"])
    calls = {}
    for ins in (f.mlil.instructions if f else []):
        if ins.operation == bn.MediumLevelILOperation.MLIL_CALL and ins.dest.value.type in (
                bn.RegisterValueType.ConstantPointerValue, bn.RegisterValueType.ConstantValue):
            callee = bv.get_symbol_at(ins.dest.value.value)
            if callee is not None:
                calls[callee.name] = len(ins.params)
    expected = {name: n for name, n in ROM_CALLS.items()}
    check(calls == expected, f"{label}: rom_calls's ROM calls have their prototypes' arguments ({calls})")
    text = hlil_text(f) if f else ""
    field = "g_rom_flashchip.chip_size" if chip == "esp32" else "rom_spiflash_legacy_data->chip.chip_size"
    check(field in text, f"{label}: the ROM's flash chip structure shows its fields ({field})")
    memset = bv.get_function_at(syms["memset"])
    cc = "ilp32" if chip in RISCV_CHIPS else "windowed"
    check(memset is not None and memset.calling_convention is not None and memset.calling_convention.name == cc
          and memset.symbol.type == bn.SymbolType.ImportedFunctionSymbol and len(memset.parameter_vars) == 3,
          f"{label}: ROM memset is a typed {cc} function ({memset and memset.type})")
    clobbered = set(memset.clobbered_regs) if memset else set()
    callers = set(memset.calling_convention.caller_saved_regs) if memset and memset.calling_convention else {None}
    check(clobbered == callers, f"{label}: ROM memset clobbers the caller-saved registers ({sorted(clobbered)})")
    regions = rom_regions(bv)
    check(len(regions) == 1 and regions[0].start == 0x40000000, f"{label}: one ROM code region ({regions})")
    stray = [f.name for f in rom_functions(bv) if f.symbol.type != bn.SymbolType.ImportedFunctionSymbol
             or f.name.startswith("sub_")]
    check(not stray, f"{label}: the ROM region holds only the typed ROM functions ({stray[:5]})")


for chip, chip_id in ROM_TYPED_CHIPS.items():
    elf = os.path.join(DATA, f"idf_{chip}.elf")
    syms = elf_symbols(elf)
    for label, bv in ((f"idf_{chip}.elf", bn.load(elf)),
                      (f"idf_{chip} image", load_bytes(elf_to_image(elf, chip_id), ".bin"))):
        check_rom_types(bv, chip, syms, label)
        functions, rom = len(bv.functions), len(rom_functions(bv))
        db = os.path.join(user_dir, f"rom_types_{chip}_{label.split()[-1]}.bndb")
        bv.create_database(db)
        bv.file.close()
        for reopen in ("reopened", "reopened again"):
            bv = bn.load(db)
            bv.update_analysis_and_wait()
            check_rom_types(bv, chip, syms, f"{label} {reopen}")
            check(len(bv.functions) == functions and len(rom_functions(bv)) == rom,
                  f"{label} {reopen}: no new functions ({len(bv.functions)} of {functions}, "
                  f"{len(rom_functions(bv))} of {rom} in the ROM)")
            bv.file.save_auto_snapshot()
            bv.file.close()

# The ESP32 v3.0 ROM's Secure Boot v2 API is typed only for images that
# require v3.0.
elf = os.path.join(DATA, "idf_esp32.elf")
for min_rev, present in ((0, False), (300, True)):
    bv = load_bytes(elf_to_image(elf, 0, min_rev), ".bin")
    f = bv.get_function_at(0x4000f8d4)  # ets_use_secure_boot_v2
    check((f is not None and f.name == "ets_use_secure_boot_v2" and str(f.return_type) == "bool") == present,
          f"ESP32 v{min_rev // 100}.{min_rev % 100} image: ets_use_secure_boot_v2 {'typed' if present else 'absent'}")
# ROM contents the image maps itself (here a segment inside the ESP32's ROM)
# are not hidden by a ROM region of zeros.
rom_code = bytes.fromhex("3641001df0000000")  # entry a1, 32; retw.n
img = struct.pack("<BBBBI", 0xE9, 2, 2, 0, 0x40080000) + bytes(16)
for addr in (0x40080000, 0x40001000):
    img += struct.pack("<II", addr, len(rom_code)) + rom_code
bv = load_bytes(img, ".bin")
check(bv.view_type == "ESP" and not rom_regions(bv) and bv.read(0x40001000, 8) == rom_code,
      "an image mapping part of the ROM gets no ROM region over it")
# The ESP8266 has no ROM types, and so no ROM region.
bv = bn.load(os.path.join(DATA, "esp8266.elf"))
check(not rom_regions(bv), "esp8266.elf: no ROM code region")


def c5_psram_elf():
    """A C5 ELF with `.flash.rodata` (WA, with contents) and a NOLOAD
    `.ext_ram.bss` in the flash window, which the C5 shares with PSRAM."""
    code = struct.pack("<II", 0x00100513, 0x00008067)  # li a0, 1; ret
    rodata = bytes(range(16))
    shstrtab = b"\0.text\0.flash.rodata\0.ext_ram.bss\0.shstrtab\0"
    text_off, rodata_off = 0x100, 0x108
    shstrtab_off = rodata_off + len(rodata)
    shoff = (shstrtab_off + len(shstrtab) + 3) & ~3
    header = b"\x7fELF\x01\x01\x01" + bytes(9) + struct.pack(
        "<HHIIIIIHHHHHH", 2, 243, 1, 0x42000020, 52, shoff, 0x1, 52, 32, 3, 40, 5, 4)
    phdrs = b"".join(struct.pack("<IIIIIIII", *p) for p in [
        (1, text_off, 0x42000020, 0x42000020, len(code), len(code), 5, 4),
        (1, rodata_off, 0x42000040, 0x42000040, len(rodata), len(rodata), 6, 4),
        (1, shstrtab_off, 0x42800000, 0x42800000, 0, 0x100, 6, 4),
    ])
    sections = bytes(40) + b"".join(struct.pack("<IIIIIIIIII", *s) for s in [
        (1, 1, 6, 0x42000020, text_off, len(code), 0, 0, 4, 0),      # PROGBITS, AX
        (7, 1, 3, 0x42000040, rodata_off, len(rodata), 0, 0, 4, 0),  # PROGBITS, WA
        (21, 8, 3, 0x42800000, shstrtab_off, 0x100, 0, 0, 4, 0),     # NOBITS, WA
        (34, 3, 0, 0, shstrtab_off, len(shstrtab), 0, 0, 1, 0),
    ])
    out = header + phdrs
    out += bytes(text_off - len(out)) + code + rodata + shstrtab
    return out + bytes(shoff - len(out)) + sections


path = os.path.join(user_dir, "c5_psram.elf")
with open(path, "wb") as f:
    f.write(c5_psram_elf())
bv = bn.load(path, options={"loader.platform": "esp32c5"})
rodata = bv.get_section_by_name(".flash.rodata")
ext_ram = bv.get_section_by_name(".ext_ram.bss")
check(bv.platform.name == "esp32c5" and rodata is not None
      and rodata.semantics == bn.SectionSemantics.ReadOnlyDataSectionSemantics,
      "C5 ELF: .flash.rodata in the shared window is read-only data")
check(ext_ram is not None and ext_ram.semantics != bn.SectionSemantics.ReadOnlyDataSectionSemantics,
      f"C5 ELF: PSRAM .ext_ram.bss in the shared window stays writable ({ext_ram and ext_ram.semantics})")
seg = bv.get_segment_at(0x42800000)
check(seg is not None and seg.writable, "C5 ELF: the PSRAM segment stays writable")
bv.file.close()

# Revision-specific ROM data in RAM that the ELF names (g_ble_lll_rfmgmt_env_p
# is only in the C2 v2.0 ROM's table) is defined; revision-specific symbols the
# ELF does not carry (AES_FSb_ptr, also v2.0 only) are not. The one C3 symbol,
# listed by two C3 revision tables, must not tie the C2's.
c2_rom = [("GetSecurityInfoProc", 0x400000B0), ("MMU_Get_Page_Mode", 0x40000700),
          ("wDevCheckBlockError", 0x400017B4), ("g_ble_lll_rfmgmt_env_p", 0x3FCDF3CC)]
path = os.path.join(user_dir, "c2_rom.elf")
with open(path, "wb") as f:
    f.write(riscv_elf(0x40380000, abs_symbols=c2_rom))
bv = bn.load(path)
check(bv.platform.name == "esp32c2", f"C2 ROM symbols pick the ESP32-C2 over a doubly listed C3 one ({bv.platform})")
sym = bv.get_symbol_by_raw_name("g_ble_lll_rfmgmt_env_p")
check(sym is not None and sym.address == 0x3FCDF3CC and sym.type == bn.SymbolType.DataSymbol,
      f"C2 ELF: revision-specific ROM data it names is defined ({sym})")
check(bv.get_symbol_by_raw_name("AES_FSb_ptr") is None,
      "C2 ELF: revision-specific ROM symbols it lacks are not defined")
bv.file.close()

print("Malformed and unusual images:")


def tiny_image(segments, entry=0x40080000, count=None):
    out = struct.pack("<BBBBI", 0xE9, count or len(segments), 2, 0, entry) + bytes(16)
    for addr, body in segments:
        out += struct.pack("<II", addr, len(body)) + body
    return out


code = bytes.fromhex("3641001df0")  # entry a1, 32; retw.n
magic = struct.pack("<I", 0xABCD5432)
bv = load_bytes(tiny_image([(0x40080000, code), (0x3F400020, magic)]), ".bin")
check(bv.view_type == "ESP" and bv.get_symbol_by_raw_name("esp_app_desc") is None,
      "truncated app descriptor at EOF is ignored")
bv = load_bytes(tiny_image([(0x40080000, code), (0x3F400020, magic), (0x3FFB0000, b"A" * 512)]), ".bin")
check(bv.view_type == "ESP" and bv.get_symbol_by_raw_name("esp_app_desc") is None,
      "truncated app descriptor does not borrow the next segment")
bv = load_bytes(tiny_image([(0x40080000, code), (0x3F400020, magic + bytes(252))]), ".bin")
check(bv.get_symbol_by_raw_name("esp_app_desc") is not None, "complete app descriptor is recognized")
bv = load_bytes(tiny_image([(0x40080000, bytes.fromhex("3df00000"))]), ".bin")
check(bv.view_type == "ESP" and bv.platform.name == "esp32", "ESP32 image with a call0 entry stub")
bv = load_bytes(tiny_image([(0x40080000, code + bytes(3) + bytes.fromhex("364100"))]), ".bin")
check(bv.get_function_at(0x40080008) is not None, "entry at the very end of a segment is found")

print("User calling conventions on functions without entry:")
# entry a1, 32; movi a10, 1; movi a11, 2; movi a12, 3; call8 stub; retw.n
# and at 0x40080014 the stub, a bare 4-byte ret.n (like the placeholders
# for unresolved externals in a partially linked image, which windowed code
# calls).
caller = bytes.fromhex("364100" "a2a001" "b2a002" "c2a003" "650000" "1df0") + bytes(3) + bytes.fromhex("0df00000")
for how in ["type", "calling_convention"]:
    bv = load_bytes(tiny_image([(0x40080000, caller)]), ".bin")
    bv.update_analysis_and_wait()
    stub = bv.get_function_at(0x40080014)
    if how == "type":
        check(stub is not None and stub.calling_convention.name == "call0", "a function without entry defaults to call0")
    if stub is None:
        check(False, f"stub found ({how})")
        continue
    windowed = bv.arch.calling_conventions["windowed"]
    if how == "type":
        stub.type = bv.parse_type_string('int32_t __convention("windowed") stub(int32_t a, int32_t b, int32_t c)')[0]
    else:
        stub.calling_convention = windowed
    bv.update_analysis_and_wait()
    check(stub.calling_convention.name == "windowed", f"user-set windowed convention survives reanalysis ({how})")
    if how == "type":
        start = bv.get_function_at(0x40080000)
        text = "\n".join(str(i) for i in start.mlil.instructions) if start else ""
        check("(1, 2, 3)" in text, "the windowed stub's arguments come from a10..a12")
bv = load_bytes(tiny_image([(0x40080000, caller[:-2])]), ".bin")
bv.update_analysis_and_wait()
stub = bv.get_function_at(0x40080014)
check(stub is not None and stub.calling_convention.name == "call0", "a ret.n at the very end of a segment is call0 too")

print("Patching:")


def first_insn(bv, pred):
    """The address and text of the first instruction whose text satisfies `pred`."""
    for f in sorted(bv.functions, key=lambda f: f.start):
        for tokens, addr in f.instructions:
            text = "".join(str(t) for t in tokens)
            if pred(text.split()[0]):
                return addr, text
    return None, None


def patched(bv, addr, patch, length):
    """The disassembly after `patch`ing `addr`, restoring the bytes after."""
    orig = bv.read(addr, length)
    ok = patch(addr)
    text = bv.get_disassembly(addr)
    bv.write(addr, orig)
    return ok, text.split()[0] if text else None


bv = bn.load(esp32_elf)
check(not bv.arch.can_assemble, "assembling is not offered")
addr, text = first_insn(bv, lambda m: m in ("beqz", "bnez", "blti", "bgei", "bbci", "bbsi", "beq", "bne"))
check(addr is not None, f"a wide branch to patch: {text}")
if addr is not None:
    check(bv.is_invert_branch_patch_available(addr) and bv.is_always_branch_patch_available(addr)
          and bv.is_never_branch_patch_available(addr), f"{text}: every branch patch offered")
    ok, mnemonic = patched(bv, addr, bv.invert_branch, 3)
    check(ok and mnemonic.startswith("b") and mnemonic != text.split()[0], f"{text}: inverted to {mnemonic}")
    check(patched(bv, addr, bv.always_branch, 3) == (True, "j"), f"{text}: always branch is j")
    check(patched(bv, addr, bv.never_branch, 3) == (True, "nop"), f"{text}: never branch is nop")
addr, text = first_insn(bv, lambda m: m in ("beqz.n", "bnez.n"))
if addr is not None:
    check(not bv.is_always_branch_patch_available(addr), f"{text}: no always branch for a narrow branch")
    ok, mnemonic = patched(bv, addr, bv.invert_branch, 2)
    check(ok and {mnemonic, text.split()[0]} == {"beqz.n", "bnez.n"}, f"{text}: inverted to {mnemonic}")
    check(patched(bv, addr, bv.never_branch, 2) == (True, "nop.n"), f"{text}: never branch is nop.n")
addr, text = first_insn(bv, lambda m: m in ("call8", "callx8"))
check(addr is not None and bv.is_skip_and_return_value_patch_available(addr), f"{text}: skip and return offered")
if addr is not None:
    orig = bv.read(addr, 3)
    check(bv.skip_and_return_value(addr, 42) and bv.get_disassembly(addr).split() == ["movi", "a10,", "42"],
          f"{text}: skip and return 42 is movi a10, 42")
    bv.write(addr, orig)
addr, text = first_insn(bv, lambda m: m == "entry")
check(addr is not None and not bv.is_invert_branch_patch_available(addr)
      and not bv.is_never_branch_patch_available(addr), "entry is not a branch to patch")


print("Relocatable objects:")
OBJ = os.path.join(DATA, "obj")


def elf_sections(path):
    """An ELF32 file's contents, its sections (name -> (file offset, size,
    type)) and the names of its undefined symbols."""
    data = open(path, "rb").read()
    shoff, = struct.unpack_from("<I", data, 0x20)
    shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 0x2E)
    headers = [struct.unpack_from("<IIIIIIIIII", data, shoff + i * shentsize) for i in range(shnum)]

    def string(table, off):
        start = headers[table][4] + off
        return data[start:data.index(b"\0", start)].decode()

    sections = {string(shstrndx, sh[0]): (sh[4], sh[5], sh[1]) for sh in headers}
    undefined = set()
    for sh in headers:
        if sh[1] == 2:  # SHT_SYMTAB
            for off in range(sh[4] + 16, sh[4] + sh[5], 16):
                name, _, _, _, _, shndx = struct.unpack_from("<IIIBBH", data, off)
                if shndx == 0 and name:
                    undefined.add(string(sh[6], name))
    return data, sections, undefined


def objdump_relocs(path):
    """(section, offset, type, symbol, addend) of every relocation objdump -r
    lists (tests/data/obj/*.relocs, see scripts/build_test_elfs.sh)."""
    out, section = [], None
    for line in open(path):
        if line.startswith("RELOCATION RECORDS FOR ["):
            section = line[len("RELOCATION RECORDS FOR ["):line.index("]")]
            continue
        fields = line.split()
        if len(fields) != 3 or not fields[1].startswith("R_XTENSA_"):
            continue
        value, addend = fields[2], 0
        for sign in "+-":
            if sign in value[1:]:
                value, n = value.rsplit(sign, 1)
                addend = int(n, 16) * (1 if sign == "+" else -1)
        out.append((section, int(fields[0], 16), fields[1], value, addend))
    return out


ADDRESS_TOKENS = (bn.InstructionTextTokenType.CodeRelativeAddressToken,
                  bn.InstructionTextTokenType.PossibleAddressToken)


def operand_target(bv, addr):
    """The code target or literal address of the instruction at `addr`."""
    tokens, _ = bv.arch.get_instruction_text(bv.read(addr, 4), addr)
    values = [t.value for t in tokens if t.type in ADDRESS_TOKENS]
    return values[-1] if values else None


def relocated_values(bv, name, raw, sections):
    """The value at each R_XTENSA_32 and the target of each SLOT0_OP
    relocation objdump lists for an object ({(section, offset): value}), its
    count of each, and those that differ from what objdump says."""
    def symbol_address(sym):
        if sym in bv.sections:
            return bv.sections[sym].start
        s = bv.get_symbol_by_raw_name(sym)
        return s.address if s else None

    values, counts, bad = {}, {}, []
    for section, offset, kind, sym, addend in objdump_relocs(os.path.join(OBJ, f"{name}.relocs")):
        if section not in bv.sections:
            continue  # not loaded: .xt.prop, .xt.lit
        addr = bv.sections[section].start + offset
        target = (symbol_address(sym) + addend) & 0xFFFFFFFF
        if kind == "R_XTENSA_SLOT0_OP":
            got, want = operand_target(bv, addr), target
        elif kind == "R_XTENSA_32":
            # The word's original contents are part of the addend.
            inplace, = struct.unpack_from("<I", raw, sections[section][0] + offset)
            got, want = bv.read_int(addr, 4, False), (inplace + target) & 0xFFFFFFFF
        else:
            continue
        values[(section, offset)] = got
        counts[kind] = counts.get(kind, 0) + 1
        if got != want:
            bad.append(f"{section}+{offset:#x} {kind} {sym}{addend:+#x}: {got and hex(got)} != {want:#x}")
    return values, counts, bad


def check_object(name, platform):
    path = os.path.join(OBJ, f"{name}.o")
    raw, sections, undefined = elf_sections(path)
    bv = bn.load(path, options={"loader.platform": platform} if platform else {})
    chip = platform or ("esp8266" if name.startswith("esp8266") else "esp32")
    check(bv.view_type == "ELF" and bv.platform.name == chip, f"{name}: loads as {bv.platform}")

    _, counts, bad = relocated_values(bv, name, raw, sections)
    check(not bad and counts.get("R_XTENSA_SLOT0_OP") and counts.get("R_XTENSA_32"),
          f"{name}: {counts} relocations match objdump" + (f" ({len(bad)} differ: {bad[:3]})" if bad else ""))

    # Undefined symbols are external, and code refers to them by name: by a
    # direct call, or (with -mlongcalls) a call through a literal.
    extern = bv.get_section_by_name(".extern")
    externs = [bv.get_symbol_by_raw_name(u) for u in sorted(undefined)]
    check(externs and all(s is not None and s.type == bn.SymbolType.ExternalSymbol
                          and extern.start <= s.address < extern.end for s in externs),
          f"{name}: undefined symbols ({', '.join(sorted(undefined)[:3])}, ...) are external")
    start = func(bv, "start")
    text = hlil_text(start) if start else ""
    calls = ["memcpy(", "add3(1, 2, 3)", "many(1, 2, 3, 4, 5, 6, 7, 8)", "fp(4, 5, 6)"]
    # The ESP8266's cas tail-calls libgcc's compare-and-swap through a
    # literal; external, it says nothing of cas's parameters.
    if chip != "esp8266":
        calls.append("cas(&sink, 1, 2)")
    for call in calls:
        check(call in text, f"{name}: start calls {call}...")
    if "longcalls" not in name and "idf" not in name:
        lines = [str(line).split() for bb in start.basic_blocks for line in bb.get_disassembly_text()] if start else []
        check(any(words[:1] in (["call0"], ["call8"]) and words[-1] == "memcpy" for words in lines),
              f"{name}: the direct call to memcpy shows its name")
    return bv


for chip in ["esp32", "esp32s2", "esp32s3", "esp8266"]:
    for variant in ["", "-longcalls"]:
        check_object(f"{chip}{variant}", None if chip in ("esp32", "esp8266") else chip)
bv = check_object("esp32-idf", None)
check(".literal.start" in bv.sections and bv.sections[".literal.start"].start < bv.sections[".text.start"].start,
      "esp32-idf: per-function literal sections lie before their code")

# R_XTENSA_32 adds to the word in place, so it must only ever be applied to
# the file's bytes. Binary Ninja reapplies a section's relocations when it is
# written to, and on reopening a database and rebasing.
path = os.path.join(OBJ, "esp32.o")
raw, sections, _ = elf_sections(path)
bv = bn.load(path)
values, _, _ = relocated_values(bv, "esp32", raw, sections)


def unchanged(bv, delta=0):
    """Whether the relocations are where objdump says, `delta` from where
    they were on loading, or what differs."""
    now, _, bad = relocated_values(bv, "esp32", raw, sections)
    moved = {k: (v + delta) & 0xFFFFFFFF for k, v in values.items()}
    return not bad and now == moved, f" ({bad[:3]})" if bad else ""


literal = bv.sections[".literal"]
edit = next(a for a in range(literal.end - 1, literal.start, -1) if not bv.range_contains_relocation(a, 1))
byte = bv.read(edit, 1)
with bv.undoable_transaction():
    bv.write(edit, bytes([byte[0] ^ 0xff]))
edited, why = unchanged(bv)
bv.undo()
undone, why2 = unchanged(bv)
check(edited and undone and bv.read(edit, 1) == byte,
      "esp32: relocations unchanged by an edit next to them and its undo" + why + why2)
db = os.path.join(user_dir, "esp32-obj.bndb")
check(bv.create_database(db), "esp32: saved to a database")
bv.file.close()
bv = bn.load(db)
ok, why = unchanged(bv)
check(ok, "esp32: relocations unchanged on reopening the database" + why)
delta = 0x100000
rebased = bv.rebase(bv.start + delta)
ok, why = unchanged(rebased, delta) if rebased else (False, " (not rebased)")
check(ok, "esp32: relocations move with a rebase" + why)
for v in (rebased, bv):
    if v:
        v.file.close()

# reloc_edge.S: relocations needing an encoding the instruction lacks are left
# as assembled, the control cases relocated.
path = os.path.join(OBJ, "reloc_edge.o")
raw, sections, _ = elf_sections(path)
bv = bn.load(path)
text_off = sections[".text.edge"][0]
edge = bv.sections[".text.edge"].start
late = bv.sections[".late"].start
far = bv.sections[".far"].start
check(bv.sections[".early"].start < edge < late < far, "reloc_edge: sections laid out in header order")
for off, what in [(0x3, "l32r of a later literal"), (0x6, "misaligned call8"), (0x9, "call8 out of range"),
                  (0xc, "j out of range"), (0xf, "beqz out of range"), (0x12, "backwards bnez.n")]:
    size = 2 if off == 0x12 else 3
    check(bv.read(edge + off, size) == raw[text_off + off:text_off + off + size], f"reloc_edge: {what} left as assembled")
check(operand_target(bv, edge + 0x14) == bv.sections[".early"].start, "reloc_edge: l32r of an earlier literal relocated")
check(operand_target(bv, edge + 0x17) == late + 4, "reloc_edge: call8 in range relocated")

# A static library's members open through Binary Ninja's AR container
# transform (bn.load does not open an archive by itself).
from binaryninja import transform  # noqa: E402

session = transform.TransformSession(os.path.join(OBJ, "libcorpus.a"))
check(session.process() and session.root_context.available_transforms == ["AR"], "libcorpus.a: an AR container")
members = {c.filename: c for c in session.root_context.children}
check(sorted(members) == ["esp32-longcalls.o", "esp8266.o"], f"libcorpus.a: members {sorted(members)}")
for member, chip in [("esp32-longcalls.o", "esp32"), ("esp8266.o", "esp8266")]:
    if member in members:
        bv = bn.load(members[member].input)
        add3 = func(bv, "add3")
        check(bv.platform.name == chip and bv.relocation_ranges and add3 is not None
              and bv.get_functions_by_name("start") and "add3(1, 2, 3)" in hlil_text(func(bv, "start")),
              f"libcorpus.a: {member} loads as {bv.platform} with its relocations applied")

print()
if failures:
    print(f"{len(failures)} failure(s)")
    sys.exit(1)
print("all checks passed")
