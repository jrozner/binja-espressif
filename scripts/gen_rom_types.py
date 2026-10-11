#!/usr/bin/env python3
"""Generates data/rom_types/<chip>.bntl: a Binary Ninja type library for each
chip's mask ROM, with the prototypes of its functions and the types of its
data, from ESP-IDF's ROM headers and newlib's headers.

The plugin embeds the libraries and types the ROM functions an image or ELF
calls with them (see src/espressif/rom_types.rs), so that their call sites
decompile with the right arguments and return values.

Usage: scripts/gen_rom_types.py [--plugin LIB] HEADERS CHIP...
  HEADERS is the directory scripts/fetch_rom_type_headers.sh fills from the
  espressif/idf docker image: ESP-IDF's components (headers and ROM linker
  scripts), the toolchains' newlib headers and each chip's default
  sdkconfig.h. LIB is the plugin build that registers the chip platforms
  (default: target/release/libbinja_xtensa.so). Runs Binary Ninja from
  ~/binaryninja (or $BN_INSTALL_DIR) with a temporary user directory, as
  tests/test_headless.py does.

  The libraries were generated from ESP-IDF v5.4.1 (IDF_VERSION below):
    scripts/fetch_rom_type_headers.sh ../rom-headers
    scripts/gen_rom_types.py ../rom-headers esp32 esp32s2 esp32s3 \\
        esp32c2 esp32c3 esp32c6 esp32h2 esp32c5

What goes into a chip's library:

* The headers (see headers()) are each parsed as the main source file,
  since Binary Ninja's parser only returns the declarations of the main
  file, and the results merged; the first declaration of a name wins. They
  are the chip's components/esp_rom/<chip>/include/<chip>/rom/*.h, esp_rom's
  chip-neutral headers, the Xtensa runtime's or the RISC-V interrupt API's
  headers, newlib's headers for the C library functions the ROM provides,
  and scripts/rom_types_extra.h for the libgcc helpers and the few others
  no installed header declares. The parser is given the chip's target
  triple (without it the RISC-V platforms parse as i386 and lay out 64-bit
  fields wrongly), newlib's headers in place of the host's, the chip's
  default sdkconfig.h, and the include directories of an ESP-IDF build.
* Names: every name of a ROM function or variable of the chip that has a
  declaration: those of src/espressif/rom_symbols.rs (of any chip revision),
  those of the ESP-IDF version's ROM linker scripts and its esp_rom API
  aliases (`esp_rom_printf = ets_printf`), which ELF files carry. Names
  without a declaration take that of another name at the same address, and
  a `<name>_rom` ROM function the prototype of `<name>` (ESP-IDF wraps those
  in inline functions of the plain name). Variables declared as arrays of
  unknown size are left out: the parser makes them pointers.
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
OUT = os.path.join(ROOT, "data", "rom_types")
ROM_SYMBOLS = os.path.join(ROOT, "src", "espressif", "rom_symbols.rs")
EXTRA = os.path.join(ROOT, "scripts", "rom_types_extra.h")

IDF_VERSION = "v5.4.1"

CHIPS = ["esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c6", "esp32h2", "esp32c5"]
XTENSA = {"esp32", "esp32s2", "esp32s3"}

# The ROM's code on the Xtensa chips: the chip's "rom" region in
# src/espressif/image.rs. The RISC-V chips' ranges are read from
# rom_symbols.rs (<CHIP>_ROM_CODE).
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

# The RISC-V interrupt controller API, which the RISC-V ROMs implement
# (riscv/ld/rom.api.ld), relative to components/riscv/include.
RISCV_HEADERS = ["riscv/interrupt.h"]

# Lines removed from a header parsed as the main file: includes that lead
# back to the header itself, which `#pragma once` does not stop for the
# main file, so its types would be defined twice.
SOURCE_FIXUPS = {"riscv/include/riscv/interrupt.h": ['#include "esp_private/interrupt_deprecated.h"']}

# Kconfig values the parser is given in place of the default sdkconfig's, so
# that the headers declare the functions of the later revisions' ROMs too
# (the loader only uses those for images that require such a revision).
CONFIG_OVERRIDES = {"esp32": {"CONFIG_ESP32_REV_MIN_FULL": "300"}}

# esp_rom headers that are not about the ROM, or that need code the parser
# cannot see.
SKIP_HEADERS = {"esp_rom_libc_stubs.h"}


def sdkconfig_dir(idf, chip, work):
    """A directory with the chip's sdkconfig.h, with CONFIG_OVERRIDES."""
    text = open(os.path.join(idf, "sdkconfig", chip, "sdkconfig.h")).read()
    for name, value in CONFIG_OVERRIDES.get(chip, {}).items():
        text, n = re.subn(rf"^#define {name} .*$", f"#define {name} {value}", text, flags=re.M)
        if n != 1:
            sys.exit(f"{chip}: {name} is not in sdkconfig.h")
    d = os.path.join(work, chip)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "sdkconfig.h"), "w") as f:
        f.write(text)
    return d


def include_dirs(idf, chip, work):
    c = os.path.join(idf, "components")
    dirs = [
        sdkconfig_dir(idf, chip, work),
        f"{c}/newlib/platform_include",
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
    arch, arch_headers = ("xtensa", XTENSA_HEADERS) if chip in XTENSA else ("riscv", RISCV_HEADERS)
    for h in arch_headers:
        out.append((os.path.join(idf, "components", arch, "include", h), f"{arch}/include/{h}"))
    for h in NEWLIB_HEADERS:
        out.append((os.path.join(newlib_dir(idf, chip), h), "newlib/" + h))
    out.append((EXTRA, "scripts/rom_types_extra.h"))
    return out


def rom_tables():
    """{chip: ({name: addr} of every revision's tables, [(start, end)] code)}
    from rom_symbols.rs."""
    text = open(ROM_SYMBOLS).read()
    out = {}
    for m in re.finditer(r"pub static (\w+): &\[\(&str, u32\)\] = &\[(.*?)\];", text, re.S):
        # ESP32, ESP32_ECO3, ESP32C3_ECO7, ...
        chip = m.group(1).split("_")[0].lower()
        if chip not in CHIPS:
            continue
        syms = out.setdefault(chip, ({}, []))[0]
        for name, addr in re.findall(r'\("([^"]+)", (0x[0-9a-f]+)\)', m.group(2)):
            syms.setdefault(name, int(addr, 16))
    for m in re.finditer(r"pub static (\w+)_ROM_CODE: &\[\(u32, u32\)\] = &\[(.*?)\];", text, re.S):
        chip = m.group(1).lower()
        if chip in out:
            out[chip][1].extend((int(s, 16), int(e, 16)) for s, e in
                                re.findall(r"\((0x[0-9a-f]+), (0x[0-9a-f]+)\)", m.group(2)))
    for chip, rom in XTENSA_ROM.items():
        if chip in out:
            out[chip][1].append(rom)
    return out


LD_SYMBOL = re.compile(r"^\s*(?:PROVIDE\s*\(\s*)?([A-Za-z_][\w$]*)\s*=\s*(0x[0-9a-fA-F]+|[A-Za-z_]\w*)\s*\)?\s*;",
                       re.M)


def ld_symbols(path):
    """The `name = 0xaddress;` and `name = other_name;` assignments of a
    linker script, as (name, address or name)."""
    text = re.sub(r"/\*.*?\*/", "", open(path).read(), flags=re.S)
    return [(name, int(value, 16) if value.startswith("0x") else value) for name, value in LD_SYMBOL.findall(text)]


def rom_names(idf, chip, table):
    """{name: address} of every name of the chip's ROM functions and data:
    those of rom_symbols.rs, then those of the ESP-IDF version's ROM linker
    scripts (which may name more, or name them differently: ELF files carry
    those), then the esp_rom API aliases (`PROVIDE ( esp_rom_printf =
    ets_printf );`, in <chip>.rom.api.ld and, on the RISC-V chips,
    riscv/ld/rom.api.ld)."""
    names = dict(table)
    ld = os.path.join(idf, "components", "esp_rom", chip, "ld")
    api = [os.path.join(ld, f"{chip}.rom.api.ld")]
    if chip not in XTENSA:
        api.append(os.path.join(idf, "components", "riscv", "ld", "rom.api.ld"))
    for f in sorted(os.listdir(ld)):
        path = os.path.join(ld, f)
        if f.endswith(".ld") and path not in api:
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
        source = open(path).read()
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
    The aliases of a name are the other names at its address, and for
    `<name>_rom`, `<name>`. Names in the ROM's code are functions."""
    at = {}
    for name, addr in names.items():
        at.setdefault(addr, []).append(name)

    objects = {}
    for name, addr in sorted(names.items()):
        is_function = any(s <= addr < e for s, e in code)
        decls = functions if is_function else variables
        others = sorted(n for n in at[addr] if n != name)
        candidates = [name] + ([name[:-4]] if name.endswith("_rom") else []) + others
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
    ap.add_argument("headers")
    ap.add_argument("chips", nargs="+")
    args = ap.parse_args()
    for chip in args.chips:
        if chip not in CHIPS:
            sys.exit(f"unknown chip {chip} (one of {', '.join(CHIPS)})")
    bn = setup_binaryninja(os.path.abspath(args.plugin))
    tables = rom_tables()
    os.makedirs(OUT, exist_ok=True)
    for chip in args.chips:
        if bn.Platform[chip] is None:
            sys.exit(f"platform {chip} is not registered: is {args.plugin} the plugin?")
        functions, variables, types, used = parse_chip(bn, args.headers, chip, args.verbose)
        table, code = tables[chip]
        objects = assign(rom_names(args.headers, chip, table), code, functions, variables)
        lib = build_library(bn, chip, objects, types, used)
        path = os.path.join(OUT, f"{chip}.bntl")
        lib.write_to_file(path)
        rom_funcs = [n for n, a in table.items() if any(s <= a < e for s, e in code)]
        typed = sum(1 for n in rom_funcs if n in objects)
        data = sum(1 for o in objects.values() if o[0] == "data")
        print(f"{chip}: {typed}/{len(rom_funcs)} of rom_symbols.rs's ROM functions typed "
              f"({100 * typed / len(rom_funcs):.0f}%), {data} variables, {len(objects)} objects in all, "
              f"{os.path.getsize(path)} bytes")
        if args.dump:
            os.makedirs(args.dump, exist_ok=True)
            with open(os.path.join(args.dump, f"{chip}.txt"), "w") as f:
                for name, (kind, t, via) in sorted(objects.items()):
                    f.write(f"{kind} {name}: {t}" + (f"  [via {via}]" if via != name else "") + "\n")


main()
