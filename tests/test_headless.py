#!/usr/bin/env python3
"""End-to-end tests of the plugin in headless Binary Ninja.

Loads the test ELFs (tests/data/*.elf, see scripts/build_test_elfs.sh) and
firmware images synthesized from them, and checks platform detection, the
calling conventions, call-site arguments, zero-overhead loop recovery and the
ESP image loader.

Usage: tests/test_headless.py [path/to/libbinja_xtensa.so]

Runs Binary Ninja from ~/binaryninja (or $BN_INSTALL_DIR) with a temporary
user directory that contains only this plugin and a link to your license.
"""
import os
import struct
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DATA = os.path.join(ROOT, "tests", "data")
plugin = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "release", "libbinja_xtensa.so"))
install = os.environ.get("BN_INSTALL_DIR", os.path.expanduser("~/binaryninja"))

user_dir = tempfile.mkdtemp(prefix="bn-xtensa-test-")
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

print()
if failures:
    print(f"{len(failures)} failure(s)")
    sys.exit(1)
print("all checks passed")
