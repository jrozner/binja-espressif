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

The mask ROM image checks need Espressif's esp-rom-elfs release archive (the
one pinned in src/espressif/rom_images.rs): set $BN_XTENSA_ROM_TARBALL to
its path, or they are skipped. The other checks run without ROM images, as
an ESP-IDF install's ROM ELF files are hidden from the plugin.
"""
import atexit
import hashlib
import http.server
import os
import re
import shutil
import struct
import sys
import tarfile
import tempfile
import threading
import time
import zipfile

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
# The plugin looks for ESP-IDF's ROM ELF files in $IDF_TOOLS_PATH and
# ~/.espressif: none of those here.
os.environ.pop("IDF_TOOLS_PATH", None)
os.environ["HOME"] = os.environ["USERPROFILE"] = user_dir
sys.path.insert(0, os.path.join(install, "python"))

import binaryninja as bn  # noqa: E402

# The plugin's warnings and errors, for the checks of what it logs.
log_path = os.path.join(user_dir, "log.txt")
bn.log_to_file(bn.LogLevel.WarningLog, log_path)

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
            check(bv.view_type == "ESP" and bv.platform.name == chip and bv.file.display_name == "factory",
                  f"flash dump based at {base:#x}: its factory app")

print("ESP8266 flash dumps:")
esp8266_image = elf_to_image(os.path.join(DATA, "esp8266.elf"), None)
# An RTOS SDK app longer than 64 KiB, in its partition: not a NONOS dump.
rtos = flash_dump(esp8266_image, 0) + b"\xff" * 0x20000
bv = load_bytes(rtos, ".bin")
check(bv.view_type == "ESP" and bv.file.is_container_entry and "flash" not in bv.sections,
      "with a partition table, the app partition is opened, without the flash cache window")
db = os.path.join(user_dir, "esp8266_app.bndb")
check(bv.create_database(db), "the app partition saves to a database")
bv.file.close()
bv = bn.load(db)
check(bv.view_type == "ESP" and "flash" not in bv.sections, "nor once its database is reopened")
bv.file.close()
# NONOS: the app at 0, its irom0text.bin flashed at 0x10000.
nonos = bytearray(b"\xff" * 0x20000)
nonos[:len(esp8266_image)] = esp8266_image
nonos[0x10000:0x10004] = bytes.fromhex("12c1f000")
bv = load_bytes(bytes(nonos), ".bin")
check("flash" in bv.sections, "without one (NONOS), the flash cache window is mapped")

print("Flash dumps as containers:")


def stub_image(entry):
    """A one-segment ESP32 image: `entry a1, 32; retw.n` at `entry`."""
    body = bytes.fromhex("3641001df0") + bytes(3)
    return struct.pack("<BBBBI", 0xE9, 1, 2, 0x20, entry) + bytes(16) + struct.pack("<II", entry, len(body)) + body


def table_entry(kind, subtype, offset, size, label):
    return struct.pack("<2sBBII16sI", b"\xaa\x50", kind, subtype, offset, size, label.encode(), 0)


# Based at 0x1000 like `esptool merge_bin` output: bootloader at file offset
# 0. The factory app is tests/data/esp32.elf; ota_1 is past the end of the
# dump.
esp32_elf = os.path.join(DATA, "esp32.elf")
dump = bytearray(flash_dump(elf_to_image(esp32_elf, 0), 0x1000))
dump[:0x40] = stub_image(0x40080400).ljust(0x40, b"\xff")
table = (table_entry(0, 0x10, 0x110000, 0x1000, "ota_0") + table_entry(0, 0x11, 0x200000, 0x1000, "ota_1")
         + table_entry(1, 2, 0x9000, 0x6000, "nvs"))
dump[0x8000 - 0x1000 + 32:0x8000 - 0x1000 + 32 + len(table)] = table
dump = dump.ljust(0x110000 - 0x1000, b"\xff") + stub_image(0x40080800).ljust(0x1000, b"\xff")
dump_path = os.path.join(user_dir, "partitions.bin")
with open(dump_path, "wb") as f:
    f.write(dump)
esp32_entry = struct.unpack_from("<I", open(esp32_elf, "rb").read(), 0x18)[0]

bv = bn.load(dump_path)
check(bv.view_type == "ESP" and bv.entry_point == esp32_entry, "default: the factory app")
check(bv.file.is_container_entry and bv.file.virtual_path == f"EspFlash({dump_path})::factory"
      and bv.file.display_name == "factory", f"named as a file of the dump ({bv.file.virtual_path})")
provenance = {"name": "factory", "virtualPath": f"EspFlash({dump_path})::factory", "label": "factory",
              "type": 0, "subtype": 0, "flashOffset": 0x10000, "size": 0x100000}
check(bv.query_metadata("espressif.flash.entry") == provenance, "where in flash it came from is in its metadata")
for choice, entry in [("factory", esp32_entry), ("ota_0", 0x40080800), ("bootloader", 0x40080400)]:
    bv = bn.load(dump_path, options={"files.espressif.partition": choice})
    check(bv.view_type == "ESP" and bv.entry_point == entry and bv.file.display_name == choice,
          f"files.espressif.partition = {choice}")
for choice in ("nvs", "partition-table"):
    bv = bn.load(dump_path, options={"files.espressif.partition": choice})
    check(bv.view_type != "ESP" and bv.file.display_name == choice, f"{choice} opens as data")


def whole_dump(bv):
    return bv.view_type == "ESP" and not bv.file.is_container_entry and bv.file.virtual_path == dump_path


for choice in ("*", "nope", "ota_1"):
    bv = bn.load(dump_path, options={"files.espressif.partition": choice})
    check(whole_dump(bv) and bv.entry_point == esp32_entry, f"files.espressif.partition = {choice}: the whole dump")

# The files of the dump, as the UI's container browser lists them. (A
# session has to outlive the views of its files.)
session = bn.TransformSession(dump_path, options={"files.espressif.partition": "*"})
session.process()
files = {c.filename: c for c in session.root_context.children}
check(sorted(files) == ["bootloader", "factory", "nvs", "ota_0", "ota_1", "partition-table"],
      f"a session lists the bootloader, partition table and partitions ({sorted(files)})")
ota0 = files.get("ota_0")
check(ota0 is not None and ota0.extraction_message == "app/ota_0 partition at 0x110000, 0x1000 bytes"
      and ota0.metadata_obj.value == {"label": "ota_0", "type": 0, "subtype": 0x10, "flashOffset": 0x110000,
                                      "size": 0x1000},
      "a file's description and metadata")
boot = files.get("bootloader")
check(boot is not None and boot.metadata_obj.value == {"flashOffset": 0x1000, "size": 0x7000},
      "the bootloader is at 0x1000")
ota1 = files.get("ota_1")
check(ota1 is not None and ota1.extraction_result != bn.TransformResult.TransformSuccess
      and ota1.extraction_message.endswith("not in the dump"), "a partition past the end of the dump is not extracted")
del session, files, ota0, boot, ota1

# Without the container, the view loads one image of the dump, chosen by
# the same setting.
excluded = {"files.container.excludedTransforms": ["EspFlash"]}
bv = bn.load(dump_path, options=excluded)
check(whole_dump(bv) and bv.entry_point == esp32_entry, "without the container: the dump's factory app")
settings = bn.Settings()
for choice, entry in [("ota_0", 0x40080800), ("bootloader", 0x40080400)]:
    settings.set_string("files.espressif.partition", choice)
    bv = bn.load(dump_path, options=excluded)
    check(whole_dump(bv) and bv.entry_point == entry, f"without the container: files.espressif.partition = {choice}")
settings.set_string("files.espressif.partition", "nvs")
bv = bn.load(dump_path, options=excluded)
check(bv.view_type != "ESP", "without the container, a partition that is not an app is not loaded")
settings.reset("files.espressif.partition")

# A file of the dump keeps where it came from in its database.
bv = bn.load(dump_path, options={"files.espressif.partition": "ota_0"})
db = os.path.join(user_dir, "ota_0.bndb")
check(bv.create_database(db), "a file of the dump saves to a database")
bv.file.close()
bv = bn.load(db)
md = bv.query_metadata("espressif.flash.entry")
check(bv.view_type == "ESP" and bv.entry_point == 0x40080800 and md["label"] == "ota_0"
      and md["flashOffset"] == 0x110000 and md["virtualPath"] == f"EspFlash({dump_path})::ota_0",
      "reopened, its database keeps where it came from")
bv.file.close()

# Where a file came from is told by its path and contents: the same app at
# different offsets in two dumps, a label with the path separator in it,
# and a dump inside a zip archive.
def small_dump(parts, images):
    """A dump from flash 0 of `(type, subtype, offset, size, label)`
    partitions and `(flash offset, image)` images."""
    d = bytearray(b"\xff" * 0x30000)
    t = b"".join(table_entry(*p) for p in parts)
    d[0x8000:0x8000 + len(t)] = t
    for offset, image in images:
        d[offset:offset + len(image)] = image
    return bytes(d)


app = stub_image(0x40080800)
dump_a = small_dump([(0, 0, 0x10000, 0x1000, "factory")], [(0x10000, app)])
dump_b = small_dump([(1, 2, 0x9000, 0x6000, "nvs"), (0, 0, 0x20000, 0x1000, "factory"),
                     (0, 0x10, 0x21000, 0x1000, "ota::0")], [(0x20000, app), (0x21000, app)])
for name, blob, choice, offset in [("a", dump_a, "factory", 0x10000), ("b", dump_b, "factory", 0x20000),
                                   ("b", dump_b, "ota::0", 0x21000)]:
    path = os.path.join(user_dir, f"dump_{name}.bin")
    with open(path, "wb") as f:
        f.write(blob)
    bv = bn.load(path, options={"files.espressif.partition": choice})
    md = bv.query_metadata("espressif.flash.entry")
    check(bv.view_type == "ESP" and md.get("label") == choice and md.get("flashOffset") == offset,
          f"{choice} of dump {name} is at {offset:#x}")
    bv.file.close()
zip_path = os.path.join(user_dir, "dump_b.zip")
with zipfile.ZipFile(zip_path, "w") as z:
    z.writestr("dump_b.bin", dump_b)
bv = bn.load(zip_path, options={"files.espressif.partition": "factory"})
md = bv.query_metadata("espressif.flash.entry") if bv is not None else {}
check(bv is not None and bv.view_type == "ESP" and bv.file.virtual_path.endswith("EspFlash(dump_b.bin)::factory")
      and md.get("flashOffset") == 0x20000, "a dump in a zip archive")
bv.file.close()

# An ESP8266 v2 image in a partition: the flash cache maps its irom0 code
# from where the partition is in flash.
v2_body = struct.pack("<II", 0x40100000, 8) + bytes.fromhex("12c1f000") + bytes(4)
v2_image = (struct.pack("<BBBBI", 0xEA, 4, 0, 0, 0x40100004) + struct.pack("<II", 0, 8) + bytes.fromhex("12c1f000")
            + bytes(4) + struct.pack("<BBBBI", 0xE9, 1, 0, 0, 0x40100004) + v2_body)
v2_path = os.path.join(user_dir, "esp8266_v2.bin")
with open(v2_path, "wb") as f:
    f.write(small_dump([(0, 0, 0x11000, 0x10000, "factory")], [(0x11000, v2_image)]))
bv = bn.load(v2_path)
check(bv.view_type == "ESP" and bv.get_segment_at(0x40211010) is not None and bv.get_segment_at(0x40200010) is None,
      "an ESP8266 v2 app partition at 0x11000 maps its irom0 code at 0x40211010")
db = os.path.join(user_dir, "esp8266_v2.bndb")
bv.create_database(db)
bv.file.close()
bv = bn.load(db)
check(bv.get_segment_at(0x40211010) is not None, "... also once its database is reopened")
bv.file.close()

# Single images (a partition read with `esptool.py read_flash` or
# `parttool.py read_partition`, or an image from a build) are not
# containers: the ESP view opens them on their own, with the core's load
# settings, whatever files.espressif.partition says.
esp_flash = bn.Transform["EspFlash"]
factory_part = bytes(dump[0x10000 - 0x1000:0x110000 - 0x1000])
standalone = [
    ("an app image", elf_to_image(esp32_elf, 0), esp32_entry, False),
    ("a bootloader image", stub_image(0x40080400), 0x40080400, False),
    ("the factory partition cut out of the dump", factory_part, esp32_entry, False),
    ("an ESP8266 app image", esp8266_image, None, False),
    ("an ESP8266 app partition longer than 64 KiB", esp8266_image.ljust(0x30000, b"\xff"), None, False),
    ("a NONOS flash dump", bytes(nonos), None, True),
]
for what, blob, entry, window in standalone:
    bv = load_bytes(blob, ".bin")
    raw = bn.BinaryView.new(blob)
    load_settings = bn.BinaryViewType["ESP"].get_load_settings_for_data(bv.parent_view)
    check(bv.view_type == "ESP" and not bv.file.is_container_entry and not esp_flash.can_decode(raw)
          and (entry is None or bv.entry_point == entry) and ("flash" in bv.sections) == window
          and "espressif.flash.entry" not in bv.metadata
          and load_settings is not None and load_settings.contains("loader.platform"),
          f"{what} opens on its own" + (", with the flash cache window" if window else ""))
    bv.file.close()
part_path = os.path.join(user_dir, "factory_partition.bin")
with open(part_path, "wb") as f:
    f.write(factory_part)
bv = bn.load(part_path, options={"files.espressif.partition": "ota_1"})
check(bv.view_type == "ESP" and bv.entry_point == esp32_entry and not bv.file.is_container_entry,
      "a single partition ignores files.espressif.partition")
bv.file.close()
settings.set_string("files.espressif.partition", "ota_1")
bv = bn.load(part_path)
check(bv.view_type == "ESP" and bv.entry_point == esp32_entry, "... also set globally")
bv.file.close()
settings.reset("files.espressif.partition")
session = bn.TransformSession(part_path)
session.process()
check(not session.root_context.children, "a session has no files for a single partition")
del session

# A flash dump in a project.
project = bn.Project.create_project(os.path.join(user_dir, "project"), "flash dumps")
project_file = project.create_file_from_path(dump_path, None, "partitions.bin")
bv = bn.load(project_file)
check(bv is not None and bv.view_type == "ESP" and bv.entry_point == esp32_entry and bv.file.display_name == "factory",
      "a dump in a project opens its factory app")
bv.file.close()
project.close()

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
    # An ordinary argument is not read from a register the site does not
    # set: it and those after it go on the stack.
    got = layout(cc, [I32] * 3, permitted=[r[0], r[2], r[3]])
    check(got == [r[0], "sp+0", "sp+4"], f"{cc_name} f(int, int, int) at a call site setting {r[0]}, "
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
bv.update_analysis_and_wait()
check(bv.view_type == "ESP" and bv.platform.name == "esp32c5", "esp32c5: ESP view")
# The C5's SVD is vendored from esp-bist (espressif/svd has none); the base
# addresses are ESP-IDF's (soc/esp32c5/register/soc/reg_base.h).
c5_peripherals = {
    0x60000000: "UART0", 0x60001000: "UART1", 0x60008000: "TIMG0", 0x60081000: "SPI2",
    0x60091000: "GPIO", 0x60096000: "PCR", 0x600B1400: "LP_UART", 0x600B4800: "EFUSE",
    0x20000000: "CLINT", 0x20800000: "CLIC",
}
for addr, name in c5_peripherals.items():
    check({s.name for s in bv.get_symbols(addr, 1)} == {name}, f"esp32c5: bundled SVD names {name} at {addr:#x}")
    check(bv.get_segment_at(addr) is not None, f"esp32c5: {name} is mapped")
var = bv.get_data_var_at(0x60091000)
check(var is not None and str(var.type) == "struct GPIO", f"esp32c5: bundled SVD types GPIO ({var and var.type})")
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


def riscv_elf(vaddr, flags=0x1, abs_symbols=(), memsz=None, machine=243):
    """A minimal executable RV32 ELF: `li a0, 1; ret` loaded at `vaddr`,
    with `e_flags` = `flags` (0x1 = RVC, soft-float) and a symbol table of
    `ABS` symbols. `memsz` makes the segment that much bigger in memory.
    `machine` = 94 makes it an Xtensa file, with the same bytes."""
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
        "<HHIIIIIHHHHHH", 2, machine, 1, vaddr, 52, shoff, flags, 52, 32, 1, 40, 5, 4)
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


ROM_DATA_REGION = "espressif.rom.data"


def rom_regions(bv):
    """The ROM code regions (not the ROM's data bus alias)."""
    return [r for r in bv.memory_map.regions if r.name.startswith("espressif.rom") and r.name != ROM_DATA_REGION]


def rom_elf_path(image):
    return next(p for p in (os.path.join(d, image) for d in ROM_ELF_DIRS) if os.path.exists(p))


def rom_elf_rodata(image):
    """The address and bytes of the read-only data a ROM ELF file loads on
    the data bus (at a virtual address other than its physical one), from
    the first such byte to the last, or None."""
    data = open(rom_elf_path(image), "rb").read()
    phoff, = struct.unpack_from("<I", data, 0x1C)
    phentsize, phnum = struct.unpack_from("<HH", data, 0x2A)
    segs = []
    for i in range(phnum):
        kind, off, vaddr, paddr, filesz, _, flags, _ = struct.unpack_from("<8I", data, phoff + i * phentsize)
        if kind == 1 and filesz and vaddr != paddr and not flags & 2 and 0x3FF00000 <= vaddr < 0x3FFB0000:
            segs.append((vaddr, data[off:off + filesz]))
    if not segs:
        return None
    start = min(a for a, _ in segs)
    out = bytearray(max(a + len(b) for a, b in segs) - start)
    for a, b in segs:
        out[a - start:a - start + len(b)] = b
    return start, bytes(out)


def check_rom_data(bv, label, image=None):
    """With `image`, the ROM's data bus alias holds the ROM ELF file's
    .rodata (if the chip has one); without, it is not mapped."""
    region = next((r for r in bv.memory_map.regions if r.name == ROM_DATA_REGION), None)
    rodata = rom_elf_rodata(image) if image else None
    if rodata is None:
        check(region is None, f"{label}: no ROM data region ({region})")
        return
    start, data = rodata
    check(region is not None and (region.start, region.length) == (start, len(data))
          and bv.read(start, len(data)) == data,
          f"{label}: the ROM's data bus alias at {start:#x} holds {image}'s .rodata ({region})")
    seg = bv.get_segment_at(start)
    check(seg is not None and seg.readable and not seg.writable and not seg.executable,
          f"{label}: the ROM's data is read-only data ({seg})")


def rom_functions(bv):
    return [f for f in bv.functions if bv.memory_map.get_active_memory_region_at(f.start).startswith("espressif.rom")]


def rom_status(bv):
    """The view's espressif.rom metadata, or None."""
    return bv.query_metadata("espressif.rom") if "espressif.rom" in bv.metadata else None


def check_rom_status(bv, chip, label, image=None, status=None, **fields):
    """espressif.rom says the ROM region holds `image` (else zeros, or
    `status`), with `fields`, and no absolute path."""
    meta = rom_status(bv)
    want = {"status": status or ("mapped" if image else "zeros"), "chip": chip, **fields}
    if image:
        want.update(image=image, revision=int(re.search(r"_rev(\d+)_", image).group(1)))
    keys = {"status", "chip"} | ({"image", "revision", "source", "release", "sha256"} if image else set())
    ok = (isinstance(meta, dict) and all(meta.get(k) == v for k, v in want.items())
          and set(meta) <= keys and set(meta) >= keys - {"release"}
          and not any(isinstance(v, str) and os.sep in v for v in meta.values()))
    if ok and image:
        ok = re.fullmatch(r"[0-9a-f]{64}", meta["sha256"]) is not None
    check(ok, f"{label}: espressif.rom is {want} ({meta})")
    return meta


def check_rom_types(bv, chip, syms, label, image=None):
    """The ROM calls in rom_calls (at its address in `syms`) and their
    callees. With `image`, the ROM ELF file the ROM region holds, else
    zeros."""
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
    held = bv.query_metadata("espressif.rom.image") if "espressif.rom.image" in bv.metadata else None
    check(held == image, f"{label}: the ROM region holds {image or 'zeros'} ({held})")
    check_rom_data(bv, label, image)
    check_rom_status(bv, chip, label, image)
    code = bv.read(syms["memset"], 4)
    check((code != bytes(4)) == bool(image), f"{label}: ROM memset's code is {'real' if image else 'zeros'} ({code.hex()})")
    if not image:
        stray = [f.name for f in rom_functions(bv) if f.symbol.type != bn.SymbolType.ImportedFunctionSymbol
                 or f.name.startswith("sub_")]
        check(not stray, f"{label}: the ROM region holds only the typed ROM functions ({stray[:5]})")
        return
    # With real bodies, analysis must not change the typed functions: no
    # thunk names over the ROM's, no narrowed parameters, no __pure.
    check(memset is not None and memset.name == "memset" and str(memset.type.parameters[0].type) == "void*"
          and not memset.is_pure.value, f"{label}: ROM memset keeps its name and prototype ({memset and memset.type})")
    renamed = [f.name for f in rom_functions(bv) if f.name.startswith("j_")
               and any(s.type == bn.SymbolType.ImportedFunctionSymbol for s in bv.get_symbols(f.start, 1))]
    check(not renamed, f"{label}: no typed ROM function is renamed as a thunk ({renamed[:5]})")
    check_rom_image_functions(bv, label, image, syms["memset"])


# The directories the ROM image checks' ROM ELF files are in.
ROM_ELF_DIRS = []


def rom_elf_functions(image):
    """address -> name of the functions (STT_FUNC) a ROM ELF file's symbol
    table names, from the file in ROM_ELF_DIRS."""
    data = open(rom_elf_path(image), "rb").read()
    shoff, = struct.unpack_from("<I", data, 0x20)
    shentsize, shnum = struct.unpack_from("<HH", data, 0x2E)
    sections = [struct.unpack_from("<IIIIIIIIII", data, shoff + i * shentsize) for i in range(shnum)]
    out = {}
    for sh in sections:
        if sh[1] != 2:  # SHT_SYMTAB
            continue
        strtab = sections[sh[6]]
        for off in range(sh[4], sh[4] + sh[5], 16):
            name, value, _, info, _, shndx = struct.unpack_from("<IIIBBH", data, off)
            if info & 0xF == 2 and shndx != 0xFFF1:  # STT_FUNC, not SHN_ABS
                end = data.index(b"\0", strtab[4] + name)
                out.setdefault(value, data[strtab[4] + name:end].decode())
    return out


def check_rom_image_functions(bv, label, image, memset, created=True):
    """Every function the ROM ELF file `image` names in the ROM region is a
    function of the view, analyzed, with a function symbol, unless
    espressif.rom.createFunctions is off (`created`): then only some are,
    and the others are only named."""
    named = {a: n for a, n in rom_elf_functions(image).items()
             if (bv.memory_map.get_active_memory_region_at(a) or "").startswith("espressif.rom")}
    missing = [n for a, n in named.items() if bv.get_function_at(a) is None]
    unnamed = [n for a, n in named.items() if bv.get_symbol_at(a) is None]
    kinds = {bv.get_symbol_at(a).type for a in named if bv.get_symbol_at(a) is not None}
    check(not unnamed and kinds <= {bn.SymbolType.FunctionSymbol, bn.SymbolType.ImportedFunctionSymbol},
          f"{label}: the ROM's {len(named)} functions have function symbols ({unnamed[:5]}, {kinds})")
    if not created:
        check(missing and len(rom_functions(bv)) < len(named),
              f"{label}: createFunctions off: {len(rom_functions(bv))} of the ROM's {len(named)} functions")
        return
    check(len(named) > 1000 and not missing,
          f"{label}: the ROM's {len(named)} functions are functions ({len(missing)} not: {missing[:5]})")
    # One the application does not call, and one it does: analyzed.
    uncalled = next((a for a in named if bv.get_symbol_at(a) is not None
                     and bv.get_symbol_at(a).type == bn.SymbolType.FunctionSymbol and not list(bv.get_code_refs(a))), None)
    for what, addr in (("an uncalled ROM function", uncalled), ("ROM memset", memset)):
        f = bv.get_function_at(addr) if addr is not None else None
        check(f is not None and not f.analysis_skipped and len(f.llil) > 0 and len(f.mlil) > 0,
              f"{label}: {what} ({f and f.name}) is analyzed")


def check_rom_types_saved(chip, chip_id, image=None):
    """check_rom_types on the ELF file and an image of it, and on databases
    of them reopened twice."""
    elf = os.path.join(DATA, f"idf_{chip}.elf")
    syms = elf_symbols(elf)
    for label, bv in ((f"idf_{chip}.elf", bn.load(elf)),
                      (f"idf_{chip} image", load_bytes(elf_to_image(elf, chip_id), ".bin"))):
        held = image and image(label)
        check_rom_types(bv, chip, syms, label, held)
        functions, rom = len(bv.functions), len(rom_functions(bv))
        code = bv.read(0x40000000, 0x1000)
        db = os.path.join(user_dir, f"rom_types_{chip}_{label.split()[-1]}{'_image' if image else ''}.bndb")
        bv.create_database(db)
        bv.file.close()
        for reopen in ("reopened", "reopened again"):
            bv = bn.load(db)
            bv.update_analysis_and_wait()
            check_rom_types(bv, chip, syms, f"{label} {reopen}", held)
            check(len(bv.functions) == functions and len(rom_functions(bv)) == rom,
                  f"{label} {reopen}: no new functions ({len(bv.functions)} of {functions}, "
                  f"{len(rom_functions(bv))} of {rom} in the ROM)")
            check(bv.read(0x40000000, 0x1000) == code, f"{label} {reopen}: the ROM region keeps its contents")
            bv.file.save_auto_snapshot()
            bv.file.close()


for chip, chip_id in ROM_TYPED_CHIPS.items():
    check_rom_types_saved(chip, chip_id)

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
# ROM data that the ROM tables lack but the ELF names is defined where the
# ESP32-S3 sees its ROM on the data bus (0x3FF00000..0x3FF20000): _ctype_ is
# in the S3 ROM's .rodata. Past the end of that range is not ROM.
s3_rom = [("_ctype_", 0x3FF191FC), ("not_rom", 0x3FF20000)]
path = os.path.join(user_dir, "s3_rom_data.elf")
with open(path, "wb") as f:
    f.write(riscv_elf(0x42000020, flags=0x300, abs_symbols=s3_rom, machine=94))
bv = bn.load(path)
check(bv.platform.name == "esp32s3", f"an Xtensa ELF in the S3's flash is ESP32-S3 code ({bv.platform})")
sym = bv.get_symbol_by_raw_name("_ctype_")
check(sym is not None and sym.address == 0x3FF191FC and sym.type == bn.SymbolType.DataSymbol,
      f"S3 ELF: ROM data on the data bus that it names is defined ({sym})")
check(bv.get_symbol_by_raw_name("not_rom") is None, "S3 ELF: past the ROM's data bus range is not ROM")
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

print("Images of unsupported chips:")
UNSUPPORTED = "is not supported by binja-xtensa"


def chip_image(chip_id, entry=0x40800000):
    """A one-segment image with `chip_id` in its extended header."""
    ext = struct.pack("<BBBBHBHH4sB", 0xEE, 0, 0, 0, chip_id, 0, 0, 0, bytes(4), 0)
    body = bytes.fromhex("1301f1ff8280")  # addi sp, sp, -16; ret
    return struct.pack("<BBBBI", 0xE9, 1, 2, 0x20, entry) + ext + struct.pack("<II", entry, len(body)) + body


def logged(text):
    with open(log_path) as f:
        return [line for line in f if text in line]


# Headless, nobody is asked: that no ROM image is available, and what to do
# about it headless (the user settings, not load options; the command for a
# database), is a warning, once per session. So is that a database keeps its
# zeros.
hint = logged("ROM image is available, so the ROM's code region holds zeros")
check(len(hint) == 1 and all(s in hint[0] for s in ("espressif.rom.elfDirectory", "espressif.rom.autoDownload",
                                                     "user settings", "load option", "Espressif\\\\Download ROM Images"))
      and "Plugins >" not in hint[0], f"headless, the hint is a warning, once, with headless remedies ({hint})")
noted = logged("this database was saved without the")
check(len(noted) == 1 and "PluginCommand" in noted[0], f"headless, a database with zeros says how to map the image ({noted})")

for chip_id, expected in [(20, "ESP image for ESP32-C61 (chip id 20) at file offset 0x0 " + UNSUPPORTED),
                          (18, "ESP image for ESP32-P4 (chip id 18) at file offset 0x0 " + UNSUPPORTED),
                          (0x7F, "ESP image with unknown chip id 127 (0x7f) at file offset 0x0 " + UNSUPPORTED)]:
    before = len(logged(expected))
    bv = load_bytes(chip_image(chip_id), ".bin")
    # The view type is asked again, as the UI's "Open with Options" does.
    esp_view_type = bn.BinaryViewType["ESP"]
    valid = esp_view_type.is_valid_for_data(bv.file.raw)
    check(bv is not None and bv.view_type != "ESP" and not valid,
          f"chip id {chip_id:#x}: not loaded (view type {bv.view_type if bv else None})")
    check(len(logged(expected)) - before == 1, f"chip id {chip_id:#x}: says so once: {expected!r}")
    bv.file.close()

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

print("call0 code on a windowed core:")
# tests/data/call0.c built with -mabi=call0 for the ESP32: its functions get
# the call0 convention before their parameters and results are inferred, and
# callers see the results of call0 and callx0 calls.
call0_elf = os.path.join(DATA, "esp32-call0.elf")
call0_symbols = elf_symbols(call0_elf)
for label, bv in [("ELF", bn.load(call0_elf)), ("image", load_bytes(elf_to_image(call0_elf, 0), ".bin"))]:
    check(bv.platform is not None and bv.platform.name == "esp32", f"{label}: platform {bv.platform}")
    for name, nparams in [("add3", 3), ("pair", 2), ("twice", 1)]:
        f = bv.get_function_at(call0_symbols[name])
        check(f is not None and f.calling_convention.name == "call0", f"{label}: {name} uses call0")
        # Parameters outside the convention's registers would show as `@ a2`.
        check(f is not None and len(f.parameter_vars) == nparams and "@" not in str(f.type),
              f"{label}: {name} has {nparams} parameters in the call0 argument registers ({f.type if f else None})")
        check(f is not None and [str(r) for r in f.return_regs] == ["a2"], f"{label}: {name} returns in a2")
    add3 = bv.get_function_at(call0_symbols["add3"])
    check(add3 is not None and "return arg2 * arg3 + arg1" in hlil_text(add3), f"{label}: add3 returns its result")
    twice = bv.get_function_at(call0_symbols["twice"])
    callee = "add3" if label == "ELF" else f"sub_{call0_symbols['add3']:x}"
    check(twice is not None and f"return {callee}(arg1, arg1, 1) + 1" in hlil_text(twice),
          f"{label}: twice uses the result of add3, called with l32r/callx0")
    start = bv.get_function_at(call0_symbols["start"])
    text = hlil_text(start) if start else ""
    check(start is not None and len(start.parameter_vars) == 0, f"{label}: start has no parameters ({start.type if start else None})")
    check(f"= {callee}(1, 2, 3)" in text, f"{label}: start uses the result of add3, called with l32r/callx0")
    names = {"twice": "twice", "pair": "pair", "op": "op"} if label == "ELF" else {
        n: f"{'sub' if n != 'op' else 'data'}_{call0_symbols[n]:x}" for n in ("twice", "pair", "op")}
    # The result of twice is passed to pair and, kept in a12 across the call
    # through the op pointer (a callx0 with an unknown target), added to its
    # result.
    t = next((line.split(" = ")[0].split()[-1] for line in text.splitlines() if f"= {names['twice']}(" in line), None)
    check(t is not None and f"{names['pair']}({t}, 5)" in text, f"{label}: start passes the result of twice to pair")
    check(t is not None and f"{names['op']}(4, 5, 6) + {t}" in text,
          f"{label}: a callx0 through a pointer takes call0 arguments, returns in a2 and keeps a12")
    if label == "ELF":
        # A call through a typed function pointer takes the pointer's type,
        # which the core applies itself: an int64_t result in a2/a3.
        ty = bv.parse_type_string('int64_t (__convention("call0") *op)(int32_t, int32_t, int32_t)')[0]
        bv.define_user_data_var(call0_symbols["op"], ty, "op")
        bv.update_analysis_and_wait()
        check("op(4, 5, 6).d + " in hlil_text(start), "ELF: a callx0 through a typed pointer follows its type")

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

print("Mask ROM images:")


def pinned_release():
    """The esp-rom-elfs release pinned in src/espressif/rom_images.rs."""
    src = open(os.path.join(ROOT, "src", "espressif", "rom_images.rs")).read()
    body = src[src.index("pub const RELEASE: Release"):]
    return {k: re.search(rf'{k}: "([^"]+)"', body).group(1) for k in ("tag", "url", "sha256")}


def sha256_of(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def wait_for(cond, seconds=60):
    end = time.time() + seconds
    while time.time() < end:
        if cond():
            return True
        time.sleep(0.1)
    return cond()


def rom_image_checks(tarball):
    release = pinned_release()
    archive = open(tarball, "rb").read()
    check(hashlib.sha256(archive).hexdigest() == release["sha256"],
          f"{os.path.basename(tarball)} is the pinned esp-rom-elfs {release['tag']}")
    settings = bn.Settings()
    elfs = os.path.join(user_dir, "rom-elfs-configured")
    with tarfile.open(tarball) as tar:
        if hasattr(tarfile, "data_filter"):
            tar.extractall(elfs, filter="data")
        else:
            tar.extractall(elfs)

    ROM_ELF_DIRS.extend([elfs, os.path.join(user_dir, "espressif", "rom-elfs", release["tag"])])

    # Espressif's ROM ELF files opened directly: each supported chip's (the
    # lowest revision's) on its own platform, with no ROM region over its
    # ROM; another chip's (the C61's passes the C5's checks, the P4's and
    # S31's are hard-float) keeps Binary Ninja's platform.
    supported = ("esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c6", "esp32h2", "esp32c5")
    roms = {}
    for name in os.listdir(elfs):
        m = re.fullmatch(r"(\w+)_rev(\d+)_rom\.elf", name)
        if m and (m.group(1) not in roms or int(m.group(2)) < roms[m.group(1)][0]):
            roms[m.group(1)] = (int(m.group(2)), name)
    check(set(supported) <= set(roms), f"the archive has every supported chip's ROM ELF file ({sorted(roms)})")
    for chip, (_, name) in sorted(roms.items()):
        bv = bn.load(os.path.join(elfs, name))
        platform = bv.platform.name if bv.platform else None
        if chip in supported:
            arch = "rv32gc" if chip in RISCV_CHIPS else f"xtensa-{chip}"
            # The file's own segments hold the ROM's code and data: no ROM
            # code or data region over them.
            own = [r.name for r in bv.memory_map.regions if r.name.startswith("espressif.rom")]
            check(bv.view_type == "ELF" and platform == chip and bv.arch.name == arch and not own
                  and len(bv.functions) > 500,
                  f"{name} opens as {chip} ({platform}, {bv.arch.name}, {len(bv.functions)} functions, "
                  f"ROM regions {own})")
            # Its ROM functions have the ROM type library's prototypes, pinned
            # (memset, and on the chips with a jump table, its entry too).
            memsets = [(str(f.type), f.has_user_type) for f in bv.functions if f.name == "memset"]
            check(memsets and all(t == "void*(void* arg1, int32_t arg2, uint32_t arg3)" and pinned
                                  for t, pinned in memsets),
                  f"{name}: memset has its prototype ({memsets})")
            # They clobber their convention's caller-saved registers, as the
            # typed ROM functions of a firmware view do, whatever analysis
            # of a jump table entry finds, and so do the calls to them.
            pinned = [f for f in bv.functions if f.has_user_type]
            short = [f.name for f in pinned
                     if set(f.clobbered_regs) != set(f.calling_convention.caller_saved_regs)]
            check(len(pinned) > 200 and not short,
                  f"{name}: {len(pinned)} typed ROM functions clobber the caller-saved registers ({short[:5]})")
            kept = []
            sites = 0
            for f in pinned:
                if sites >= 100:
                    break
                saved = set(f.calling_convention.caller_saved_regs) - {"zero"}
                for ref in list(bv.get_code_refs(f.start))[:5]:
                    call = ref.function.get_llil_at(ref.address)
                    if call is None or call.operation != bn.LowLevelILOperation.LLIL_CALL:
                        continue
                    sites += 1
                    out = {str(r.reg) for r in call.ssa_form.output}
                    if not saved <= out:
                        kept.append((hex(ref.address), f.name, sorted(saved - out)))
            check(sites > 20 and not kept,
                  f"{name}: {sites} calls to them clobber the caller-saved registers ({kept[:3]})")
        else:
            check(platform not in supported, f"{name}, another chip's ROM, keeps {platform}")
        bv.file.close()

    # A directory of ROM ELF files: the ROM region holds the chip's ROM, and
    # the typed ROM functions, their calls and databases are as without.
    settings.set_string("espressif.rom.elfDirectory", elfs)
    for chip, chip_id in ROM_TYPED_CHIPS.items():
        check_rom_types_saved(chip, chip_id, lambda label, chip=chip: f"{chip}_rev0_rom.elf")
    elf = os.path.join(DATA, "idf_esp32.elf")
    bv = bn.load(elf)
    memset = bv.get_function_at(0x4000C44C)
    check(bv.get_disassembly(0x4000C44C).startswith("entry") and memset is not None
          and "uint32_t" in hlil_text(memset) and len(memset.basic_blocks) > 5,
          f"ESP32 ROM memset disassembles and decompiles ({bv.get_disassembly(0x4000C44C)})")
    named = bv.get_symbol_by_raw_name("_malloc_r")
    check(named is not None and 0x40000000 <= named.address < 0x40070000,
          f"ESP32 ROM functions are named from the ROM ELF file ({named})")
    # The ROM's data, on the data bus: newlib's character class table, the
    # pointer to it the ROM's scanf reads, and that read resolved.
    ctype = bytes([0]) + bytes([0x20] * 9) + bytes([0x28] * 5) + bytes([0x20] * 18) + bytes([0x88])
    check(bv.read(0x3FF96354, len(ctype)) == ctype, f"ESP32 ROM _ctype_ reads ({bv.read(0x3FF96354, 8).hex()})")
    check(bv.read(0x3FF96350, 4) == struct.pack("<I", 0x3FF96354), "ESP32 ROM __ctype_ptr__ points to _ctype_")
    readers = {r.function.name for r in bv.get_code_refs(0x3FF96350)}
    users = {r.function.name for r in bv.get_code_refs(0x3FF96354)}
    check("__svfiscanf_r" in readers and "__svfiscanf_r" in users,
          f"the ROM's scanf references __ctype_ptr__ and, through it, _ctype_ ({sorted(readers)}, {sorted(users)})")
    var = bv.get_data_var_at(0x3FF991D0)  # exc_cause_table, typed from the ROM type library
    first = struct.unpack("<I", bv.read(0x3FF991D0, 4))[0]
    check(var is not None and str(var.type) == "char const* const[0x28]"
          and bv.get_ascii_string_at(first) is not None,
          f"a typed ROM variable on the data bus has its value ({var and var.type}, {first:#x})")
    # A database saved without the ROM's data (by an earlier version) gets
    # it from the command.
    bv.memory_map.remove_memory_region(ROM_DATA_REGION)
    db = os.path.join(user_dir, "rom_data_missing.bndb")
    bv.create_database(db)
    bv.file.close()
    bv = bn.load(db)
    check(not bv.is_valid_offset(0x3FF96354), "a database saved without the ROM's data keeps it unmapped")
    bn.PluginCommand.get_valid_list(bn.PluginCommandContext(bv))["Espressif\\Download ROM Images"].execute(
        bn.PluginCommandContext(bv))
    wait_for(lambda: bv.is_valid_offset(0x3FF96354))
    check_rom_data(bv, "a database saved without the ROM's data, after the command", "esp32_rev0_rom.elf")
    bv.file.close()
    # The ESP32-S2's.
    bv = bn.load(os.path.join(DATA, "esp32s2.elf"))
    check(bv.read(0x3FFAC76C, len(ctype)) == ctype, f"ESP32-S2 ROM _ctype_ reads ({bv.read(0x3FFAC76C, 8).hex()})")
    check_rom_data(bv, "esp32s2.elf", "esp32s2_rev0_rom.elf")
    bv.file.close()
    # Without creating the ROM's functions: the ones code calls, the others
    # named.
    settings.set_bool("espressif.rom.createFunctions", False)
    bv = bn.load(elf)
    check_rom_image_functions(bv, "idf_esp32.elf", "esp32_rev0_rom.elf", 0x4000C44C, created=False)
    # One ROM function without its name, as from a ROM ELF file that does
    # not name it: the command names it from the image.
    unnamed = next(a for a, n in rom_elf_functions("esp32_rev0_rom.elf").items()
                   if bv.get_function_at(a) is None and bv.get_symbol_at(a) is not None
                   and bv.get_symbol_at(a).name == n and bv.get_symbol_at(a).auto)
    unnamed_name = bv.get_symbol_at(unnamed).name
    bv.undefine_auto_symbol(bv.get_symbol_at(unnamed))
    db = os.path.join(user_dir, "rom_functions_off.bndb")
    bv.create_database(db)
    bv.file.close()
    settings.set_bool("espressif.rom.createFunctions", True)
    # A database saved so gets them from the command, as the image is there.
    bv = bn.load(db)
    rom = len(rom_functions(bv))
    bn.PluginCommand.get_valid_list(bn.PluginCommandContext(bv))["Espressif\\Download ROM Images"].execute(
        bn.PluginCommandContext(bv))
    wait_for(lambda: len(rom_functions(bv)) > rom)
    time.sleep(1)
    wait_for(lambda: bv.analysis_progress.state == bn.AnalysisState.IdleState)
    check_rom_image_functions(bv, "a database saved with createFunctions off, after the command",
                              "esp32_rev0_rom.elf", 0x4000C44C)
    named = bv.get_symbol_at(unnamed)
    check(named is not None and named.name == unnamed_name,
          f"the command names the image's functions the database does not ({named}, {unnamed_name})")
    bv.file.close()
    # But not from another image than the one it holds: here the v3.0 ROM
    # under the v0 ROM's file name.
    other = os.path.join(user_dir, "rom-elfs-renamed")
    os.makedirs(other)
    shutil.copy(os.path.join(elfs, "esp32_rev300_rom.elf"), os.path.join(other, "esp32_rev0_rom.elf"))
    # The database keeps the elfDirectory it was loaded with (the user's,
    # then): this one's is changed, as a database's own setting.
    settings.set_string("espressif.rom.elfDirectory", other)
    bv = bn.load(db)
    kept = settings.get_string("espressif.rom.elfDirectory", bv)
    check(kept == elfs, f"a database keeps the elfDirectory it was loaded with ({kept!r})")
    settings.set_string("espressif.rom.elfDirectory", other, bv, bn.SettingsScope.SettingsResourceScope)
    rom = len(rom_functions(bv))
    bn.PluginCommand.get_valid_list(bn.PluginCommandContext(bv))["Espressif\\Download ROM Images"].execute(
        bn.PluginCommandContext(bv))
    time.sleep(3)
    wait_for(lambda: bv.analysis_progress.state == bn.AnalysisState.IdleState)
    check(len(rom_functions(bv)) == rom, f"nor from another ROM image ({len(rom_functions(bv))} of {rom})")
    bv.file.close()

    # espressif.rom.elfDirectory per file: as a load option, which a
    # database keeps, over the user's. The file under `other` is the v3.0
    # ROM named as the v0 one: only its SHA-256 tells them apart.
    v0 = sha256_of(os.path.join(elfs, "esp32_rev0_rom.elf"))
    renamed = sha256_of(os.path.join(other, "esp32_rev0_rom.elf"))
    per_file = {"espressif.rom.elfDirectory": elfs}
    settings.set_string("espressif.rom.elfDirectory", "")
    bv = bn.load(elf, options=per_file)
    check_rom_status(bv, "esp32", "elfDirectory as a load option", "esp32_rev0_rom.elf",
                     source="elfDirectory", sha256=v0)
    check(bv.read(0x4000C44C, 4) != bytes(4), "elfDirectory as a load option: the ROM's code is there")
    db = os.path.join(user_dir, "rom_elf_directory_option.bndb")
    bv.create_database(db)
    bv.file.close()
    bv = bn.load(db)
    kept = settings.get_string("espressif.rom.elfDirectory", bv)
    check(kept == elfs, f"a database keeps its elfDirectory ({kept!r})")
    bv.file.close()
    settings.set_string("espressif.rom.elfDirectory", other)
    bv = bn.load(elf)
    check_rom_status(bv, "esp32", "elfDirectory set globally", "esp32_rev0_rom.elf", source="elfDirectory",
                     sha256=renamed)
    bv.file.close()
    bv = bn.load(elf, options=per_file)
    check_rom_status(bv, "esp32", "elfDirectory as a load option over the global one", "esp32_rev0_rom.elf",
                     source="elfDirectory", sha256=v0)
    bv.file.close()
    settings.set_string("espressif.rom.elfDirectory", "")

    # From ESP-IDF's tools install: the source and esp-rom-elfs version.
    idf = os.path.join(user_dir, "idf-tools")
    os.makedirs(os.path.join(idf, "tools", "esp-rom-elfs"))
    os.symlink(elfs, os.path.join(idf, "tools", "esp-rom-elfs", "20240305"))
    os.environ["IDF_TOOLS_PATH"] = idf
    bv = bn.load(elf)
    check_rom_status(bv, "esp32", "an ESP-IDF tools install", "esp32_rev0_rom.elf", source="esp-idf",
                     release="20240305", sha256=v0)
    # A database saved before espressif.rom existed gets it from
    # espressif.rom.image.
    bv.remove_metadata("espressif.rom")
    db = os.path.join(user_dir, "rom_status_backfill.bndb")
    bv.create_database(db)
    bv.file.close()
    del os.environ["IDF_TOOLS_PATH"]
    bv = bn.load(db)
    bv.update_analysis_and_wait()
    meta = rom_status(bv)
    check(meta == {"status": "mapped", "chip": "esp32", "image": "esp32_rev0_rom.elf", "revision": 0},
          f"an older database's espressif.rom says what espressif.rom.image does ({meta})")
    bv.file.close()
    settings.set_string("espressif.rom.elfDirectory", elfs)

    # The image's revision: the minimum an image requires, the lowest for an
    # ELF file, or the setting's.
    def held(bv):
        return bv.query_metadata("espressif.rom.image") if "espressif.rom.image" in bv.metadata else None
    for min_rev, rom in ((0, "esp32_rev0_rom.elf"), (100, "esp32_rev0_rom.elf"), (300, "esp32_rev300_rom.elf")):
        bv = load_bytes(elf_to_image(elf, 0, min_rev), ".bin")
        v3_code = bv.read(0x4000F800, 4) != bytes(4)  # only in the v3.0 ROM
        check(held(bv) == rom and v3_code == (min_rev == 300),
              f"ESP32 v{min_rev // 100}.{min_rev % 100} image maps {rom} ({held(bv)})")
        bv.file.close()
    settings.set_string("espressif.rom.revision", "v3.0")
    bv = bn.load(elf)
    check(held(bv) == "esp32_rev300_rom.elf", f"espressif.rom.revision picks the ESP32 ROM v3.0 ({held(bv)})")
    bv.file.close()
    settings.set_string("espressif.rom.revision", "")
    settings.set_bool("espressif.rom.loadImages", False)
    bv = bn.load(elf)
    check(held(bv) is None and bv.read(0x4000C44C, 4) == bytes(4), "espressif.rom.loadImages off: zeros")
    bv.file.close()
    settings.set_bool("espressif.rom.loadImages", True)

    # A view loaded without an image maps one found later when reanalyzed.
    settings.set_string("espressif.rom.elfDirectory", "")
    bv = bn.load(elf)
    check(held(bv) is None, "no ROM ELF directory: zeros")
    settings.set_string("espressif.rom.elfDirectory", elfs)
    bv.update_analysis_and_wait()
    check_rom_types(bv, "esp32", elf_symbols(elf), "idf_esp32.elf reanalyzed with a ROM ELF directory",
                    "esp32_rev0_rom.elf")
    bv.file.close()

    # A calling convention the user gave a typed ROM function stays when an
    # image is mapped later.
    settings.set_string("espressif.rom.elfDirectory", "")
    bv = bn.load(elf)
    memset = bv.get_function_at(0x4000C44C)
    lib = memset.type
    memset.type = bn.FunctionType.create(lib.return_value, lib.parameters, bv.arch.calling_conventions["call0"],
                                         lib.has_variable_arguments)
    bv.update_analysis_and_wait()
    settings.set_string("espressif.rom.elfDirectory", elfs)
    bv.update_analysis_and_wait()
    memset = bv.get_function_at(0x4000C44C)
    check(held(bv) == "esp32_rev0_rom.elf" and memset.calling_convention.name == "call0"
          and str(memset.type.parameters[0].type) == "void*",
          f"a ROM function's user calling convention survives the image ({memset.type})")
    bv.file.close()

    # The download, from a local server: only with consent, verified, and
    # only the supported chips' ROM ELF files are unpacked.
    settings.set_string("espressif.rom.elfDirectory", "")
    serve = os.path.join(user_dir, "serve")
    os.makedirs(serve)
    requests = []
    delay = []

    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=serve, **kwargs)

        def do_GET(self):
            requests.append(self.path)
            time.sleep(sum(delay))
            super().do_GET()

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    name = os.path.basename(release["url"])
    os.environ["BN_XTENSA_ROM_TEST_URL"] = f"http://127.0.0.1:{server.server_address[1]}/{name}"
    with open(os.path.join(serve, name), "wb") as f:
        f.write(archive[:-1] + bytes([archive[-1] ^ 1]))  # corrupted
    cache = os.path.join(user_dir, "espressif", "rom-elfs")
    c3 = os.path.join(DATA, "idf_esp32c3.elf")
    bv = bn.load(c3)
    time.sleep(1)
    check(held(bv) is None and not requests, f"no download without consent ({requests})")
    check_rom_status(bv, "esp32c3", "no ROM image and no download")
    settings.set_bool("espressif.rom.autoDownload", True)
    bv.update_analysis_and_wait()
    wait_for(lambda: requests)
    time.sleep(2)
    check(len(requests) == 1 and not os.path.exists(cache),
          f"espressif.rom.autoDownload downloads; a corrupted archive is not unpacked ({requests})")
    wait_for(lambda: (rom_status(bv) or {}).get("status") == "failed")
    check_rom_status(bv, "esp32c3", "a failed download", status="failed")
    bv.update_analysis_and_wait()
    check_rom_status(bv, "esp32c3", "a failed download, reanalyzed", status="failed")
    with open(os.path.join(serve, name), "wb") as f:
        f.write(archive)
    commands = bn.PluginCommand.get_valid_list(bn.PluginCommandContext(bv))
    command = commands.get("Espressif\\Download ROM Images")
    check(command is not None, f"the download command is offered for an ESP32-C3 ELF ({sorted(commands)})")
    if command is not None:
        # Slowly, to see the download's status.
        delay.append(3)
        command.execute(bn.PluginCommandContext(bv))
        check(wait_for(lambda: (rom_status(bv) or {}).get("status") == "downloading", 10),
              f"espressif.rom says downloading while the command downloads ({rom_status(bv)})")
        wait_for(lambda: held(bv) is not None)
        delay.clear()
        check_rom_status(bv, "esp32c3", "after the download command", "esp32c3_rev0_rom.elf", source="cache",
                         release=release["tag"])
        # As in the UI, which does not wait for analysis: the command
        # starts it, and the ROM's functions are created and analyzed.
        time.sleep(1)
        wait_for(lambda: bv.analysis_progress.state == bn.AnalysisState.IdleState)
        check_rom_image_functions(bv, "idf_esp32c3.elf after the download command, unwaited",
                                  "esp32c3_rev0_rom.elf", elf_symbols(c3)["memset"])
        bv.update_analysis_and_wait()
        check_rom_types(bv, "esp32c3", elf_symbols(c3), "idf_esp32c3.elf after the download command",
                        "esp32c3_rev0_rom.elf")
        unpacked = sorted(os.listdir(os.path.join(cache, release["tag"]))) if os.path.exists(cache) else []
        check(len(requests) == 2 and unpacked and all(re.fullmatch(r"(\w+)_rev\d+_rom\.elf", n).group(1) in supported
                                                      for n in unpacked),
              f"the command downloads and unpacks the supported chips' ROM ELF files ({unpacked})")
    bv.file.close()
    bv = bn.load(os.path.join(DATA, "idf_esp32s3.elf"))
    check(held(bv) == "esp32s3_rev0_rom.elf" and len(requests) == 2, "the download cache serves the next file")
    check_rom_status(bv, "esp32s3", "from the download cache", "esp32s3_rev0_rom.elf", source="cache",
                     release=release["tag"],
                     sha256=sha256_of(os.path.join(cache, release["tag"], "esp32s3_rev0_rom.elf")))
    bv.file.close()
    settings.set_bool("espressif.rom.autoDownload", False)
    server.shutdown()
    del os.environ["BN_XTENSA_ROM_TEST_URL"]
    headless_download_checks(archive, release)


# A fresh Binary Ninja session, as a server like bn-remote runs it: the
# user settings turn espressif.rom.autoDownload on, the download cache is
# empty, and each file is loaded, saved to a database at once, closed and
# the database reopened. Prints the results as JSON.
SESSION = r'''
import json, os, sys, threading, time
sys.path.insert(0, os.path.join(sys.argv[1], "python"))
import binaryninja as bn
bn.log_to_file(bn.LogLevel.WarningLog, os.path.join(os.environ["BN_USER_DIRECTORY"], "log.txt"))
bn.Platform["esp32"]  # loads the plugins, before any thread loads a file
def held(bv):
    return bv.query_metadata("espressif.rom.image") if "espressif.rom.image" in bv.metadata else None
def status(bv):
    return bv.query_metadata("espressif.rom") if "espressif.rom" in bv.metadata else None
def rom_functions(bv):
    return sum(1 for f in bv.functions
               if (bv.memory_map.get_active_memory_region_at(f.start) or "").startswith("espressif.rom"))
def run(i, path, out):
    start = time.time()
    bv = bn.load(path)
    r = {"file": os.path.basename(path), "seconds": time.time() - start, "loaded": held(bv),
         "loaded_functions": rom_functions(bv), "loaded_status": status(bv)}
    db = os.path.join(os.environ["BN_USER_DIRECTORY"], f"{i}.bndb")
    bv.create_database(db)
    bv.file.close()
    bv = bn.load(db)
    r.update(reopened=held(bv), reopened_functions=rom_functions(bv), reopened_status=status(bv))
    bv.file.close()
    out[i] = r
paths = sys.argv[3:]
out = [None] * len(paths)
if sys.argv[2] == "together":
    threads = [threading.Thread(target=run, args=(i, p, out)) for i, p in enumerate(paths)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
else:
    for i, p in enumerate(paths):
        run(i, p, out)
if os.environ.get("SESSION_COMMAND"):
    # Then the command, on a new view of the first file.
    bv = bn.load(paths[0])
    start = time.time()
    ctx = bn.PluginCommandContext(bv)
    bn.PluginCommand.get_valid_list(ctx)["Espressif\\Download ROM Images"].execute(ctx)
    while held(bv) is None and time.time() - start < 20:
        time.sleep(0.1)
    out.append({"command": held(bv), "seconds": time.time() - start})
    bv.file.close()
print("RESULTS " + json.dumps(out))
'''


def headless_session(name, url, paths, together=False, env=None):
    """Runs SESSION in a Binary Ninja of its own: returns its results, and
    the plugin's warnings and errors it logged."""
    import json
    import subprocess
    ud = os.path.join(user_dir, f"session-{name}")
    os.makedirs(os.path.join(ud, "plugins"))
    os.symlink(plugin, os.path.join(ud, "plugins", os.path.basename(plugin)))
    if os.path.exists(license_file):
        os.symlink(license_file, os.path.join(ud, "license.dat"))
    with open(os.path.join(ud, "settings.json"), "w") as f:
        json.dump({"espressif.rom.autoDownload": True}, f)
    script = os.path.join(user_dir, "session.py")
    with open(script, "w") as f:
        f.write(SESSION)
    child_env = dict(os.environ, BN_USER_DIRECTORY=ud, HOME=ud, USERPROFILE=ud, BN_XTENSA_ROM_TEST_URL=url,
                     **(env or {}))
    out = subprocess.run([sys.executable, script, install, "together" if together else "apart", *paths],
                         env=child_env, capture_output=True, text=True, timeout=600)
    results = next((json.loads(line[len("RESULTS "):]) for line in out.stdout.splitlines()
                    if line.startswith("RESULTS ")), None)
    if results is None:
        print(out.stdout[-2000:], out.stderr[-2000:])
    log = open(os.path.join(ud, "log.txt")).read().splitlines() if os.path.exists(os.path.join(ud, "log.txt")) else []
    return results or [], log


def headless_download_checks(archive, release):
    """Headless, with espressif.rom.autoDownload on, a load waits for the
    download: the view that triggered it, and a database saved right after,
    has the image. A failure or a timeout leaves zeros, and says so once."""
    serve = {}
    requests = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(self.path)
            body, delays = serve["body"], serve["delays"]
            time.sleep(delays.pop(0) if delays else 0)  # the latency of a real download
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_address[1]}/{os.path.basename(release['url'])}"
    c3, esp32 = os.path.join(DATA, "idf_esp32c3.elf"), os.path.join(DATA, "idf_esp32.elf")
    c3_image = os.path.join(user_dir, "session_esp32c3.bin")
    with open(c3_image, "wb") as f:
        f.write(elf_to_image(c3, 5))

    def downloaded(name, paths, images, together=False):
        requests.clear()
        results, log = headless_session(name, url, paths, together)
        check(len(results) == len(paths) and len(requests) == 1,
              f"{name}: one download for {len(paths)} files ({len(requests)} requests)")
        for r, image in zip(results, images):
            check(r["loaded"] == image and r["loaded_functions"] > 1000,
                  f"{name}: {r['file']} has the image when bn.load returns ({r['loaded']}, "
                  f"{r['loaded_functions']} ROM functions)")
            check(r["reopened"] == image and r["reopened_functions"] == r["loaded_functions"],
                  f"{name}: {r['file']}'s database, saved at once, keeps it ({r['reopened']}, "
                  f"{r['reopened_functions']} ROM functions)")
            for when in ("loaded", "reopened"):
                st = r[f"{when}_status"] or {}
                check(st.get("status") == "mapped" and st.get("image") == image and st.get("source") == "cache"
                      and st.get("release") == release["tag"],
                      f"{name}: {r['file']} {when}: espressif.rom says mapped from the cache ({st})")
        check(not [line for line in log if "ROM image" in line], f"{name}: no ROM image warnings ({log[:3]})")

    # Over a second of latency, as from GitHub: an analysis update after the
    # load would come too late. An ESP image (the view's initialization
    # maps the ROM), then an ELF file (the ELF fix-ups do) from the cache.
    serve.update(body=archive, delays=[1.5])
    downloaded("autoDownload", [c3_image, esp32], ["esp32c3_rev0_rom.elf", "esp32_rev0_rom.elf"])
    # Two loads at once: one download, which the other waits for.
    downloaded("autoDownload, two loads at once", [c3, esp32], ["esp32c3_rev0_rom.elf", "esp32_rev0_rom.elf"],
               together=True)

    def zeros(name, env, warning, together=False):
        requests.clear()
        results, log = headless_session(name, url, [c3, esp32], together, env)
        command = results[2:]
        results = results[:2]
        check(len(results) == 2 and all(r["loaded"] is None and r["reopened"] is None for r in results)
              and len(requests) == 1 + len(command),
              f"{name}: zeros, and not downloaded again ({results}, {len(requests)} requests)")
        check(all(r["seconds"] < 20 for r in results), f"{name}: the loads end ({[r['seconds'] for r in results]})")
        statuses = [(r[f"{when}_status"] or {}).get("status") for r in results for when in ("loaded", "reopened")]
        check(statuses == ["failed"] * 4, f"{name}: espressif.rom says failed ({statuses})")
        warned = [line for line in log if warning in line]
        check(len(warned) == 1 and "holds zeros" in warned[0], f"{name}: warned once ({warned})")
        return command

    serve.update(body=archive[:-1] + bytes([archive[-1] ^ 1]), delays=[])
    zeros("a corrupted download", {}, "ROM image: the download failed")
    # Two loads at once wait for it, and give up together. The request
    # stays stuck, and the command then downloads anew instead of waiting
    # for it.
    serve.update(body=archive, delays=[15])
    command = zeros("a download that times out", {"BN_XTENSA_ROM_TEST_TIMEOUT": "2", "SESSION_COMMAND": "1"},
                    "did not finish in 2 s", together=True)
    check(command and command[0]["command"] == "esp32c3_rev0_rom.elf" and command[0]["seconds"] < 10,
          f"after a download that times out and stays stuck, the command downloads ({command})")
    server.shutdown()


if os.environ.get("BN_XTENSA_ROM_TARBALL"):
    rom_image_checks(os.environ["BN_XTENSA_ROM_TARBALL"])
else:
    print("  skipped: set BN_XTENSA_ROM_TARBALL to the esp-rom-elfs archive pinned in src/espressif/rom_images.rs")

# Each chip's ROM type library names its platform (gen_rom_types.py adds
# it before finalizing), so the platform finds it. A library loaded from a
# file is finalized: the plugin registers it as it is, as the core refuses
# (and logs an error for) any change to it.
for chip in ("esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c6", "esp32h2", "esp32c5"):
    name = f"espressif-rom-{chip}"
    platform = bn.Platform[chip]
    lib = bn.TypeLibrary.from_name(platform.arch, name)
    check(lib is not None and lib.platform_names == [chip]
          and [l.name for l in platform.get_type_libraries_by_name(name)] == [name],
          f"{name} is registered for the {chip} platform ({lib and lib.platform_names})")
immutable = logged("Attempt to mutate immutable library")
check(not immutable, f"no type library is changed after it is loaded ({immutable})")

# Images of supported chips are not reported: only the three above and the
# ESP32-P4 image of the RISC-V checks are.
check(len(logged(UNSUPPORTED)) == 4, f"no other image is reported unsupported ({logged(UNSUPPORTED)})")

print()
if failures:
    print(f"{len(failures)} failure(s)")
    sys.exit(1)
print("all checks passed")
