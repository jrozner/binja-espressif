#!/usr/bin/env python3
"""Generates view/espressif/data/rom_types/<chip>.bntl: a Binary Ninja type library for each
chip's mask ROM, with the prototypes of its functions and the types of its
data, from ESP-IDF's ROM headers and newlib's headers.

The plugin embeds the libraries and types the ROM functions an image or ELF
calls with them (see view/espressif/src/rom_types.rs), so that their call sites
decompile with the right arguments and return values.

Usage: scripts/gen_rom_types.py [--plugin LIB] [--older OLD]... HEADERS CHIP...
  HEADERS is the directory scripts/fetch_rom_type_headers.sh fills from the
  espressif/idf docker image: ESP-IDF's components (headers and ROM linker
  scripts), the toolchains' newlib headers and each chip's default
  sdkconfig.h. LIB is the plugin build that registers the chip platforms
  (default: target/release/libbinja_xtensa.so). Runs Binary Ninja from
  ~/binaryninja (or $BN_INSTALL_DIR) with a temporary user directory, as
  tests/test_headless.py does. Each OLD is an earlier ESP-IDF release's
  tree (fetch_rom_type_headers.sh DIR VERSION, or a checkout), of which
  only the ROM linker scripts and esp_idf_version.h are read.

  The libraries were generated from ESP-IDF v6.1 (IDF_VERSION below), with
  the names of v5.5.5 and v5.4.1 (the version they were generated from
  before):
    scripts/fetch_rom_type_headers.sh ../rom-headers
    scripts/fetch_rom_type_headers.sh ../rom-headers-v5.5.5 v5.5.5
    scripts/fetch_rom_type_headers.sh ../rom-headers-v5.4.1 v5.4.1
    scripts/gen_rom_types.py --older ../rom-headers-v5.5.5 \\
        --older ../rom-headers-v5.4.1 ../rom-headers esp32 esp32s2 esp32s3 \\
        esp32c2 esp32c3 esp32c6 esp32h2 esp32c5

What goes into a chip's library:

* The headers (see headers()) are each parsed as the main source file,
  since Binary Ninja's parser only returns the declarations of the main
  file, and the results merged; the first declaration of a name wins. They
  are the chip's components/esp_rom/<chip>/include/<chip>/rom/*.h, esp_rom's
  chip-neutral headers, the heap component's for the ROM's TLSF and
  multi_heap functions, the Xtensa runtime's or the RISC-V interrupt API's
  headers, newlib's headers for the C library functions the ROM provides,
  and scripts/rom_types_extra.h for the libgcc helpers and the few others
  no installed header declares. The parser is given the chip's target
  triple (without it the RISC-V platforms parse as i386 and lay out 64-bit
  fields wrongly), newlib's headers in place of the host's, the chip's
  default sdkconfig.h (configured for newlib), and the include directories
  of an ESP-IDF build.
* Names: every name of a ROM function or variable of the chip that has a
  declaration: those of view/espressif/data/rom_symbols/<chip>.txt (of any chip revision),
  those of the ESP-IDF version's ROM linker scripts and its esp_rom API
  aliases (`esp_rom_printf = ets_printf`), which ELF files carry, and those
  the older releases' (--older) give the same addresses, which ELF files
  built with them carry (see rom_names). Names without a declaration take
  that of `rom_<name>` (ESP-IDF v6 renamed the ROM's gpio_matrix_out
  rom_gpio_matrix_out) or of another name at the same address; a
  `<name>_rom` or `_rom_<name>` ROM function takes the prototype of
  `<name>` (ESP-IDF wraps those in functions of the plain name). Variables
  declared as arrays of unknown size are left out: the parser makes them
  pointers.
* Every named type the objects use, recursively.

The output is not byte-for-byte reproducible (Binary Ninja serializes the
library's contents in no fixed order), but its contents are.

The library records the ESP-IDF version and the headers it was made from in
its metadata (`espressif.idf_version`, `espressif.headers`).
"""
import argparse
import atexit
import glob
import os
import re
import shutil
import sys
import tempfile
import uuid

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "view", "espressif", "data", "rom_types")
ROM_SYMBOLS = os.path.join(ROOT, "view", "espressif", "data", "rom_symbols")
EXTRA = os.path.join(ROOT, "scripts", "rom_types_extra.h")

IDF_VERSION = "v6.1"

CHIPS = ["esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c6", "esp32h2", "esp32c5"]
XTENSA = {"esp32", "esp32s2", "esp32s3"}

# The ROM's code on the Xtensa chips: the chip's "rom" region in
# view/espressif/src/image.rs. The RISC-V chips' ranges are read from their
# ROM symbol files (`code` lines).
XTENSA_ROM = {"esp32": (0x40000000, 0x40070000), "esp32s2": (0x40000000, 0x40020000),
              "esp32s3": (0x40000000, 0x40060000)}

# newlib headers for the C library functions in the ROMs, relative to the
# toolchain's include directory. Only a header's own declarations count, so
# these are the files that declare the functions: sys/unistd.h declares read,
# write and close, sys/_default_fcntl.h open.
NEWLIB_HEADERS = ["string.h", "strings.h", "stdio.h", "stdlib.h", "ctype.h", "time.h", "sys/unistd.h",
                  "sys/_default_fcntl.h", "sys/lock.h", "reent.h", "sys/reent.h", "setjmp.h", "locale.h",
                  "sys/time.h", "sys/stat.h", "signal.h", "assert.h", "envlock.h", "malloc.h"]

# The Xtensa runtime's headers, for the Xtensa ROMs' copies of its routines
# (_xtos_set_intlevel, xthal_*), relative to components/xtensa/include.
XTENSA_HEADERS = ["xtensa/xtruntime.h", "xtensa/hal.h"]

# The declarations of the ROMs' heap (TLSF and multi_heap) functions,
# relative to components: esp_rom's esp_rom_tlsf.h and esp_rom_multi_heap.h
# until ESP-IDF v6.0.
HEAP_HEADERS = ["heap/rom_patches/include/rom_patch_tlsf.h", "heap/rom_patches/include/rom_patch_multi_heap.h"]

# The RISC-V interrupt controller API, which the RISC-V ROMs implement
# (riscv/ld/rom.api.ld), relative to components/riscv/include.
RISCV_HEADERS = ["riscv/interrupt.h"]

# Lines removed from a header parsed as the main file: includes that lead
# back to the header itself, which `#pragma once` does not stop for the
# main file, so its types would be defined twice.
SOURCE_FIXUPS = {}

# Headers replaced, wherever they are included, by a copy without the lines
# that match the pattern (and parsed without them as the main file):
# esp_rom_spiflash.h includes the chip's rom/spi_flash.h (and
# rom/opi_flash.h), which includes esp_rom_spiflash.h, so either would
# include itself as the main file. The copies go in a directory searched
# before the others.
SHADOWED_HEADERS = {"esp_rom/include/esp_rom_spiflash.h": r'^#include "esp32\w*/rom/(spi|opi)_flash\.h"$'}

# Kconfig values the parser is given in place of the default sdkconfig's, so
# that the headers declare the functions of the later revisions' ROMs too
# (the loader only uses those for images that require such a revision).
CONFIG_OVERRIDES = {"esp32": {"CONFIG_ESP32_REV_MIN_FULL": "300"}}

# The ROM scripts that ESP-IDF v6 moved out of components/esp_rom, as
# globs relative to its components directory (as in gen_rom_symbols.py).
MOVED_SCRIPTS = ["esp_hal_*/{chip}/rom.*.ld", "heap/port/{chip}/ld/{chip}.rom.*.ld"]

# The first ESP-IDF release whose ROM linker scripts describe the chip's
# ROM as the ROM symbol file does, for the chips whose earlier releases
# describe another: names are taken from older releases (--older) only from
# these on. ESP-IDF v5.4 supported the ESP32-C5's revision 0 engineering
# samples, whose ROM has other functions at some of the production (v1.0)
# ROM's addresses (sscanf where it has _printf_common); v5.5 the production
# ROM.
OLDER_MIN_VERSION = {"esp32c5": (5, 5)}

# esp_rom headers that are not about the ROM, or that need code the parser
# cannot see.
SKIP_HEADERS = {"esp_rom_libc_stubs.h"}


def sdkconfig_dir(idf, chip, work):
    """A directory with the chip's sdkconfig.h, with CONFIG_OVERRIDES, and the
    copies of SHADOWED_HEADERS."""
    text = open(os.path.join(idf, "sdkconfig", chip, "sdkconfig.h")).read()
    for name, value in CONFIG_OVERRIDES.get(chip, {}).items():
        text, n = re.subn(rf"^#define {name} .*$", f"#define {name} {value}", text, flags=re.M)
        if n != 1:
            sys.exit(f"{chip}: {name} is not in sdkconfig.h")
    d = os.path.join(work, chip)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "sdkconfig.h"), "w") as f:
        f.write(text)
    for label in SHADOWED_HEADERS:
        with open(os.path.join(d, os.path.basename(label)), "w") as f:
            f.write(shadowed(idf, label))
    return d


def shadowed(idf, label):
    """The header's source without the lines SHADOWED_HEADERS removes."""
    text = open(os.path.join(idf, "components", label)).read()
    text, n = re.subn(SHADOWED_HEADERS[label] + r"\n", "", text, flags=re.M)
    if n == 0:
        sys.exit(f"{label}: no line matches {SHADOWED_HEADERS[label]}")
    return text


def include_dirs(idf, chip, work):
    c = os.path.join(idf, "components")
    dirs = [
        sdkconfig_dir(idf, chip, work),
        f"{c}/esp_libc/platform_include",
        f"{c}/esp_rom/{chip}/include/{chip}/rom", f"{c}/esp_rom/{chip}/include/{chip}",
        f"{c}/esp_rom/{chip}/include", f"{c}/esp_rom/{chip}", f"{c}/esp_rom/include",
        f"{c}/soc/{chip}/include", f"{c}/soc/{chip}/register", f"{c}/soc/include",
        f"{c}/hal/{chip}/include", f"{c}/hal/include", f"{c}/hal/platform_port/include",
        f"{c}/esp_common/include", f"{c}/esp_hw_support/include", f"{c}/esp_hw_support/include/soc",
        f"{c}/esp_hw_support/include/esp_private", f"{c}/esp_hw_support/port/include",
        f"{c}/esp_hw_support/port/{chip}/include", f"{c}/esp_hw_support/port/{chip}/private_include",
        f"{c}/esp_system/include", f"{c}/log/include", f"{c}/heap/include",
        f"{c}/freertos/FreeRTOS-Kernel/include", f"{c}/freertos/config/include",
        f"{c}/freertos/config/include/freertos", f"{c}/esp_security/include",
        f"{c}/efuse/include", f"{c}/efuse/{chip}/include", f"{c}/spi_flash/include",
        f"{c}/bootloader_support/include",
    ]
    # ESP-IDF v6 moved the HAL's peripheral headers (hal/uart_ll.h, ...)
    # into esp_hal_<peripheral> components.
    for hal in sorted(glob.glob(f"{c}/esp_hal_*")):
        dirs += [f"{hal}/{chip}/include", f"{hal}/include"]
    if chip in XTENSA:
        dirs += [f"{c}/xtensa/include", f"{c}/xtensa/{chip}/include", f"{c}/freertos/config/xtensa/include",
                 f"{c}/freertos/FreeRTOS-Kernel/portable/xtensa/include/freertos"]
    else:
        dirs += [f"{c}/riscv/include", f"{c}/freertos/config/riscv/include",
                 f"{c}/freertos/FreeRTOS-Kernel/portable/riscv/include/freertos"]
    return [d for d in dirs if os.path.isdir(d)]


def newlib_dir(idf, chip):
    return os.path.join(idf, "newlib-xtensa" if chip in XTENSA else "newlib-riscv")


def parser_options(idf, chip):
    target = "xtensa-esp-elf" if chip in XTENSA else "riscv32-unknown-elf"
    return [f"--target={target}", "-nostdlibinc", "-isystem", newlib_dir(idf, chip),
            "-include", "sdkconfig.h", "-Dtypeof=__typeof__",
            # The parser compiles C++: the GNU dialect and _GNU_SOURCE make
            # newlib declare its extensions (itoa, memrchr, ...).
            "-std=gnu++17", "-D_GNU_SOURCE"]


def headers(idf, chip):
    """The headers parsed for the chip, as (path, name recorded in the
    library); the first declaration of a name wins."""
    c = os.path.join(idf, "components", "esp_rom")
    out = []
    for path in sorted(glob.glob(f"{c}/{chip}/include/{chip}/rom/*.h")):
        out.append((path, os.path.relpath(path, os.path.join(idf, "components"))))
    for path in sorted(glob.glob(f"{c}/include/*.h")):
        if os.path.basename(path) not in SKIP_HEADERS:
            out.append((path, os.path.relpath(path, os.path.join(idf, "components"))))
    for h in HEAP_HEADERS:
        out.append((os.path.join(idf, "components", h), h))
    arch, arch_headers = ("xtensa", XTENSA_HEADERS) if chip in XTENSA else ("riscv", RISCV_HEADERS)
    for h in arch_headers:
        out.append((os.path.join(idf, "components", arch, "include", h), f"{arch}/include/{h}"))
    for h in NEWLIB_HEADERS:
        out.append((os.path.join(newlib_dir(idf, chip), h), "newlib/" + h))
    out.append((EXTRA, "scripts/rom_types_extra.h"))
    return out


def rom_tables():
    """{chip: ({name: addr} of every revision's tables, [(start, end)] code)}
    from view/espressif/data/rom_symbols/<chip>.txt (see view/espressif/src/rom_symbols.rs for
    the format)."""
    out = {}
    for chip in CHIPS:
        syms, code = out.setdefault(chip, ({}, []))
        with open(os.path.join(ROM_SYMBOLS, f"{chip}.txt")) as f:
            for line in f:
                words = line.split()
                if not words or words[0].startswith("#") or words[0] == "revision":
                    continue
                if words[0] == "code":
                    code.append((int(words[1], 16), int(words[2], 16)))
                else:
                    syms.setdefault(words[1], int(words[0], 16))
    for chip, rom in XTENSA_ROM.items():
        out[chip][1].append(rom)
    return out


LD_SYMBOL = re.compile(r"^\s*(?:PROVIDE\s*\(\s*)?([A-Za-z_][\w$]*)\s*=\s*(0x[0-9a-fA-F]+|[A-Za-z_]\w*)\s*\)?\s*;",
                       re.M)


def ld_symbols(path):
    """The `name = 0xaddress;` and `name = other_name;` assignments of a
    linker script, as (name, address or name). A script that sets every name
    to one address is a placeholder (esp32s3.rom.ble_*.ld, see
    gen_rom_symbols.py) and gives none."""
    text = re.sub(r"/\*.*?\*/", "", open(path).read(), flags=re.S)
    syms = [(name, int(value, 16) if value.startswith("0x") else value) for name, value in LD_SYMBOL.findall(text)]
    if len(syms) > 1 and len({value for _, value in syms}) == 1 and isinstance(syms[0][1], int):
        return []
    return syms


def idf_names(idf, chip, names):
    """Adds to names ({name: address}) those of an ESP-IDF tree's ROM linker
    scripts, with the ones ESP-IDF v6 moved out of esp_rom (MOVED_SCRIPTS),
    then its esp_rom API aliases (`PROVIDE ( esp_rom_printf = ets_printf );`,
    in <chip>.rom.api.ld and, on the RISC-V chips, riscv/ld/rom.api.ld)."""
    ld = os.path.join(idf, "components", "esp_rom", chip, "ld")
    api = [os.path.join(ld, f"{chip}.rom.api.ld")]
    if chip not in XTENSA:
        api.append(os.path.join(idf, "components", "riscv", "ld", "rom.api.ld"))
    paths = [os.path.join(ld, f) for f in sorted(os.listdir(ld))]
    for pattern in MOVED_SCRIPTS:
        paths += sorted(glob.glob(os.path.join(idf, "components", pattern.format(chip=chip))))
    for path in paths:
        if path.endswith(".ld") and path not in api:
            for name, addr in ld_symbols(path):
                if isinstance(addr, int) and addr:
                    names.setdefault(name, addr)
    for path in api:
        if os.path.exists(path):
            for name, target in ld_symbols(path):
                addr = target if isinstance(target, int) else names.get(target)
                if addr:
                    names.setdefault(name, addr)
    return names


def idf_version(idf):
    """(major, minor) of an ESP-IDF tree, from esp_idf_version.h."""
    text = open(os.path.join(idf, "components", "esp_common", "include", "esp_idf_version.h")).read()
    return tuple(int(re.search(rf"#define ESP_IDF_VERSION_{part}\s+(\d+)", text).group(1)) for part in ("MAJOR", "MINOR"))


def rom_names(idf, chip, table, older=()):
    """{name: address} of every name of the chip's ROM functions and data:
    those of its ROM symbol file, then those of the ESP-IDF version's ROM linker
    scripts and API aliases (which may name more, or name them differently:
    ELF files carry those; see idf_names), then the other names that earlier
    ESP-IDF releases (`older`) give the addresses these name: ELF files built
    with those releases carry them (gpio_matrix_out, now rom_gpio_matrix_out;
    esp_rom_regi2c_read, an alias of the ROM's rom_i2c_readReg until v6.0)."""
    names = idf_names(idf, chip, dict(table))
    named = set(names.values())
    for old in older:
        if idf_version(old) < OLDER_MIN_VERSION.get(chip, (0, 0)):
            continue
        for name, addr in idf_names(old, chip, {}).items():
            if name not in names and addr in named:
                names[name] = addr
    return names



def setup_binaryninja(plugin):
    install = os.environ.get("BN_INSTALL_DIR", os.path.expanduser("~/binaryninja"))
    user_dir = tempfile.mkdtemp(prefix="bn-xtensa-romtypes-")
    atexit.register(shutil.rmtree, user_dir, True)
    os.makedirs(os.path.join(user_dir, "plugins"))
    os.symlink(plugin, os.path.join(user_dir, "plugins", os.path.basename(plugin)))
    license_file = os.path.expanduser("~/.binaryninja/license.dat")
    if os.path.exists(license_file):
        os.symlink(license_file, os.path.join(user_dir, "license.dat"))
    with open(os.path.join(user_dir, "settings.json"), "w") as f:
        f.write("{}")
    os.environ["BN_USER_DIRECTORY"] = user_dir
    sys.path.insert(0, os.path.join(install, "python"))
    import binaryninja
    return binaryninja


def parse_chip(bn, idf, chip, verbose):
    platform = bn.Platform[chip]
    work = tempfile.mkdtemp(prefix="bn-xtensa-romtypes-config-")
    atexit.register(shutil.rmtree, work, True)
    dirs = include_dirs(idf, chip, work)
    options = parser_options(idf, chip)
    functions, variables, types, used = {}, {}, {}, []
    for path, label in headers(idf, chip):
        if not os.path.exists(path):
            print(f"  {chip}: no {label}", file=sys.stderr)
            continue
        source = shadowed(idf, label) if label in SHADOWED_HEADERS else open(path).read()
        for line in SOURCE_FIXUPS.get(label, []):
            if line not in source:
                sys.exit(f"{label}: no '{line}' to remove")
            source = source.replace(line, "")
        result, errors = bn.TypeParser.default.parse_types_from_source(
            source, path, platform, None, options, dirs + [os.path.dirname(path)])
        fatal = [e for e in errors if e.severity >= bn.TypeParserErrorSeverity.ErrorSeverity]
        if result is None:
            msgs = "; ".join(f"{os.path.basename(e.file_name)}:{e.line}: {e.message}" for e in fatal[:3])
            print(f"  {chip}: {label} does not parse: {msgs}", file=sys.stderr)
            continue
        if fatal and verbose:
            msgs = "; ".join(f"{os.path.basename(e.file_name)}:{e.line}: {e.message}" for e in fatal[:3])
            print(f"  {chip}: {label}: {msgs}", file=sys.stderr)
        used.append(label)
        # The parser turns an array of unknown size (`extern const char
        # _ctype_[];`) into a pointer, which would make the variable's
        # contents an address. Those are left untyped.
        unsized = set(re.findall(r"(\w+)\s*\[\s*\]\s*;", source))
        for f in result.functions:
            functions.setdefault(str(f.name), f.type)
        for v in result.variables:
            if str(v.name) not in unsized:
                variables.setdefault(str(v.name), v.type)
        for t in result.types:
            types.setdefault(str(t.name), t.type)
    return functions, variables, types, used


def referenced_types(bn, objects, types):
    """The named types the objects use, recursively: {name: type}."""
    out = {}
    T = bn.TypeClass

    def walk(t):
        if t is None:
            return
        tc = t.type_class
        if tc == T.NamedTypeReferenceClass:
            name = str(t.name)
            if name not in out and name in types:
                out[name] = types[name]
                walk(types[name])
        elif tc == T.PointerTypeClass:
            walk(t.target)
        elif tc == T.ArrayTypeClass:
            walk(t.element_type)
        elif tc == T.FunctionTypeClass:
            walk(t.return_value)
            for p in t.parameters:
                walk(p.type)
        elif tc == T.StructureTypeClass:
            for m in t.members:
                walk(m.type)
            for b in t.base_structures:
                walk(bn.Type.named_type(b.type))

    for t in objects.values():
        walk(t)
    return out


def assign(names, code, functions, variables):
    """The library's objects, {name: (kind, type, declared as)}: the ROM's
    names (see rom_names) that have a declaration, or whose alias has one.
    The aliases of a name are `<name>` for `<name>_rom` and `_rom_<name>`,
    `rom_<name>`, and the other names at its address. Names in the ROM's
    code are functions."""
    at = {}
    for name, addr in names.items():
        at.setdefault(addr, []).append(name)

    objects = {}
    for name, addr in sorted(names.items()):
        is_function = any(s <= addr < e for s, e in code)
        decls = functions if is_function else variables
        others = sorted(n for n in at[addr] if n != name)
        candidates = ([name] + ([name[:-4]] if name.endswith("_rom") else [])
                      + ([name[5:]] if name.startswith("_rom_") else []) + [f"rom_{name}"] + others)
        via = next((n for n in candidates if n in decls), None)
        if via is not None:
            objects[name] = ("function" if is_function else "data", decls[via], via)
    return objects



def build_library(bn, chip, objects, types, used):
    platform = bn.Platform[chip]
    lib = bn.TypeLibrary.new(platform.arch, f"espressif-rom-{chip}")
    lib.guid = str(uuid.uuid5(uuid.NAMESPACE_URL, f"https://github.com/jrozner/binja-xtensa/rom-types/{chip}"))
    # The only place the platform is named: the plugin loads the finalized
    # library and registers it as it is.
    lib.add_platform(platform)
    for name, t in sorted(referenced_types(bn, {n: o[1] for n, o in objects.items()}, types).items()):
        lib.add_named_type(name, t)
    for name, (_, t, _) in sorted(objects.items()):
        lib.add_named_object(name, t)
    lib.store_metadata("espressif.idf_version", bn.Metadata(IDF_VERSION))
    lib.store_metadata("espressif.headers", bn.Metadata(used))
    lib.finalize()
    return lib


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--plugin", default=os.path.join(ROOT, "target", "release", "libbinja_xtensa.so"))
    ap.add_argument("--verbose", action="store_true", help="print the parser's errors for headers that parse")
    ap.add_argument("--dump", help="write each chip's prototypes, as text, to this directory")
    ap.add_argument("--older", action="append", default=[], metavar="HEADERS",
                    help="an earlier ESP-IDF release's headers, for the names its ROM linker scripts give")
    ap.add_argument("headers")
    ap.add_argument("chips", nargs="+")
    args = ap.parse_args()
    for chip in args.chips:
        if chip not in CHIPS:
            sys.exit(f"unknown chip {chip} (one of {', '.join(CHIPS)})")
    want = tuple(int(n) for n in IDF_VERSION.lstrip("v").split(".")[:2])
    if idf_version(args.headers) != want:
        sys.exit(f"{args.headers} is ESP-IDF {idf_version(args.headers)}, not {IDF_VERSION}")
    bn = setup_binaryninja(os.path.abspath(args.plugin))
    tables = rom_tables()
    os.makedirs(OUT, exist_ok=True)
    for chip in args.chips:
        if bn.Platform[chip] is None:
            sys.exit(f"platform {chip} is not registered: is {args.plugin} the plugin?")
        functions, variables, types, used = parse_chip(bn, args.headers, chip, args.verbose)
        table, code = tables[chip]
        objects = assign(rom_names(args.headers, chip, table, args.older), code, functions, variables)
        lib = build_library(bn, chip, objects, types, used)
        path = os.path.join(OUT, f"{chip}.bntl")
        lib.write_to_file(path)
        rom_funcs = [n for n, a in table.items() if any(s <= a < e for s, e in code)]
        typed = sum(1 for n in rom_funcs if n in objects)
        data = sum(1 for o in objects.values() if o[0] == "data")
        print(f"{chip}: {typed}/{len(rom_funcs)} of the ROM symbol file's functions typed "
              f"({100 * typed / len(rom_funcs):.0f}%), {data} variables, {len(objects)} objects in all, "
              f"{os.path.getsize(path)} bytes")
        if args.dump:
            os.makedirs(args.dump, exist_ok=True)
            with open(os.path.join(args.dump, f"{chip}.txt"), "w") as f:
                for name, (kind, t, via) in sorted(objects.items()):
                    f.write(f"{kind} {name}: {t}" + (f"  [via {via}]" if via != name else "") + "\n")


main()
