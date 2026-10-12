#!/usr/bin/env python3
"""Unit tests of the conflicting-name rule of scripts/gen_rom_symbols.py and
scripts/gen_rom_types.py, with small synthetic linker scripts and ROM ELF
files. Standard library only; no Binary Ninja, ESP-IDF or network needed.

Usage: tests/test_gen_rom.py [-v]
"""
import contextlib
import io
import os
import shutil
import struct
import sys
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "scripts"))

import gen_rom_symbols as symbols  # noqa: E402
import gen_rom_types as types  # noqa: E402

# ESP-IDF's esp32s3.rom.bt_funcs.ld gives these hooks the addresses of the
# ROM's CRC and MD5 functions, which esp32s3.rom.ld (and, since ESP-IDF
# v5.5, esp32s3.rom.newlib.ld) name.
S3_HOOKS = {
    0x40001C68: ("r_llc_rem_phy_upd_proc_continue_hook", "MD5Update"),
    0x40001C8C: ("r_lld_adv_frm_isr_hook", "hmac_md5"),
    0x40001C98: ("r_lld_con_frm_isr_hook", "crc32_le"),
    0x40001CA4: ("r_lld_con_evt_start_cbk_hook", "crc32_be"),
    0x40001CB0: ("r_lld_init_frm_eof_isr_hook", "crc16_le"),
    0x40001CBC: ("r_lld_init_sched_hook", "crc16_be"),
    0x40001CC8: ("r_lld_per_adv_frm_isr_hook", "crc8_le"),
    0x40001CD4: ("r_lld_sync_evt_start_cbk_hook", "crc8_be"),
    0x40001CE0: ("r_sch_arb_insert_hook", "esp_crc8"),
}


def elf(syms):
    """A little-endian ELF32 file with a symbol table: syms is [(name,
    address, binding)], binding 0 (local) or 1 (global), all functions."""
    strtab = b"\0"
    entries = bytes(16)
    for name, addr, binding in syms:
        entries += struct.pack("<IIIBBH", len(strtab), addr, 4, binding << 4 | 2, 0, 1)
        strtab += name.encode() + b"\0"
    text = bytes(16)
    shstrtab = b"\0.text\0.symtab\0.strtab\0.shstrtab\0"
    body = text + entries + strtab + shstrtab
    off_text, off_sym = 52, 52 + len(text)
    off_str = off_sym + len(entries)
    off_shstr = off_str + len(strtab)
    shoff = 52 + len(body)
    shoff += -shoff % 4
    ehdr = b"\x7fELF\x01\x01\x01" + bytes(9) + struct.pack(
        "<HHIIIIIHHHHHH", 2, 94, 1, 0, 0, shoff, 0, 52, 0, 0, 40, 5, 4)
    sections = [
        bytes(40),
        struct.pack("<10I", 1, 1, 6, 0x40000000, off_text, len(text), 0, 0, 4, 0),
        struct.pack("<10I", 7, 2, 0, 0, off_sym, len(entries), 3, 1, 4, 16),
        struct.pack("<10I", 15, 3, 0, 0, off_str, len(strtab), 0, 0, 1, 0),
        struct.pack("<10I", 23, 3, 0, 0, off_shstr, len(shstrtab), 0, 0, 1, 0),
    ]
    out = ehdr + body
    return out + bytes(shoff - len(out)) + b"".join(sections)


def defs(*entries):
    """A table's definitions, as collect() builds them, from (file, name,
    address)."""
    out = {}
    for f, name, addr in entries:
        out.setdefault(name, {}).setdefault(addr, []).append(f)
    return out


class ConflictingNames(unittest.TestCase):
    """gen_rom_symbols.conflicting(): the ROM ELF files decide."""

    def test_s3_hooks(self):
        d = defs(*[("esp32s3.rom.ld", rom, a) for a, (_, rom) in S3_HOOKS.items() if rom != "hmac_md5"],
                 ("esp32s3.rom.newlib.ld", "hmac_md5", 0x40001C8C),
                 *[("esp32s3.rom.bt_funcs.ld", hook, a) for a, (hook, _) in S3_HOOKS.items()])
        exports = {a: {rom} for a, (_, rom) in S3_HOOKS.items()}
        out = symbols.conflicting(d, exports)
        self.assertEqual(out, {(hook, a): [rom] for a, (hook, rom) in S3_HOOKS.items()})

    def test_aliases_from_one_script_stay(self):
        # The ESP32-S2's rom.ld names 0x40000540 _iram0_text_start and
        # _iram0_text_end; libc-funcs.ld setjmp. The ROM ELF has all three.
        d = defs(("esp32s2.rom.ld", "_iram0_text_start", 0x40000540),
                 ("esp32s2.rom.ld", "_iram0_text_end", 0x40000540),
                 ("esp32s2.rom.libc-funcs.ld", "setjmp", 0x40000540))
        exports = {0x40000540: {"_iram0_text_start", "_iram0_text_end", "setjmp"}}
        self.assertEqual(symbols.conflicting(d, exports), {})
        # An alias the ROM ELF does not have stays if its script also names
        # the address as the ROM does.
        d = defs(("a.rom.ld", "x", 0x40001000), ("a.rom.ld", "x_alias", 0x40001000), ("a.rom.bt.ld", "y", 0x40001000))
        self.assertEqual(symbols.conflicting(d, {0x40001000: {"x"}}), {("y", 0x40001000): ["x"]})

    def test_unsettled_names_stay(self):
        # The ESP32's SPI_write_enable is a local symbol of its ROM ELF
        # (not exported); esp_rom_spiflash_enable_write is ESP-IDF's name.
        d = defs(("esp32.rom.ld", "SPI_write_enable", 0x40062320),
                 ("esp32.rom.spiflash_legacy.ld", "esp_rom_spiflash_enable_write", 0x40062320))
        self.assertEqual(symbols.conflicting(d, {}), {})
        self.assertEqual(symbols.conflicting(d, {0x40062320: {"something_else"}}), {})

    def test_stubs_and_rom_copies_confirm(self):
        # The ESP32-C2 ROM's stub __call_r_mbedtls_md5_starts confirms
        # mbedtls.ld's mbedtls_md5_starts (r_ is the ROM's own copy).
        d = defs(("esp32c2.rom.ld", "mbedtls_md5_starts_ret", 0x40002BE4),
                 ("esp32c2.rom.mbedtls.ld", "mbedtls_md5_starts", 0x40002BE4))
        exports = {0x40002BE4: {"r_mbedtls_md5_starts", "mbedtls_md5_starts_ret"}}
        self.assertEqual(symbols.conflicting(d, exports), {})


class GenerateTables(unittest.TestCase):
    """gen_rom_symbols.generate() on synthetic ESP-IDF trees and ROM ELFs."""

    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="test-gen-rom-")
        self.addCleanup(shutil.rmtree, self.dir, True)
        self.elfs = os.path.join(self.dir, "elfs")
        os.makedirs(self.elfs)
        exported = [(f"__call_{rom}", a, 1) for a, (_, rom) in S3_HOOKS.items()]
        with open(os.path.join(self.elfs, "esp32s3_rev0_rom.elf"), "wb") as f:
            f.write(elf(exported + [("crc32_le", 0x40045C0C, 1), ("ets_printf", 0x40000600, 1)]))

    def tree(self, name, files):
        ld = os.path.join(self.dir, name, "components", "esp_rom", "esp32s3", "ld")
        os.makedirs(ld)
        for f, text in files.items():
            with open(os.path.join(ld, f), "w") as out:
                out.write(text)
        return os.path.dirname(os.path.dirname(ld))

    def generate(self, root, older=()):
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            lines = symbols.generate(root, "esp32s3", "test", self.elfs, older)
        return {tuple(line.split()) for line in lines if not line.startswith("#")}, err.getvalue()

    def test_hooks_are_left_out(self):
        rom = "".join(f"{rom} = {a:#x};\n" for a, (_, rom) in S3_HOOKS.items()) + "ets_printf = 0x40000600;\n"
        bt = "".join(f"{hook} = {a:#x};\n" for a, (hook, _) in S3_HOOKS.items())
        bt += "r_lld_scan_sched_hook = 0x40001c7c;\nr_lld_con_frm_isr = 0x40004254;\n"
        new = self.tree("v6.1", {"esp32s3.rom.ld": rom, "esp32s3.rom.bt_funcs.ld": bt,
                                 "esp32s3.rom.api.ld": "PROVIDE ( esp_rom_crc32_le = crc32_le );\n"})
        # ESP-IDF v5.3 has the hooks in esp32s3.rom.ld itself, and an older
        # name, which the table keeps.
        old = self.tree("v5.3", {"esp32s3.rom.ld": rom + bt + "gpio_output_set = 0x40000610;\n"})
        table, err = self.generate(new, [old])
        for a, (hook, rom) in S3_HOOKS.items():
            self.assertIn((f"{a:08x}", rom), table)
            self.assertNotIn((f"{a:08x}", hook), table)
            self.assertIn(f"left out {hook} = {a:#x} (esp32s3.rom.bt_funcs.ld), the ROM's {rom}", err)
        # Names at addresses nothing else names stay.
        for entry in [("40001c7c", "r_lld_scan_sched_hook"), ("40004254", "r_lld_con_frm_isr"),
                      ("40000600", "ets_printf"), ("40000610", "gpio_output_set")]:
            self.assertIn(entry, table)
        self.assertFalse(any(name == "esp_rom_crc32_le" for _, name in table))
        self.assertEqual(len(table), len(S3_HOOKS) + 4)


class RomTypeNames(unittest.TestCase):
    """gen_rom_types.rom_names(): names the ROM symbol file leaves out for a
    conflict get no type, from the current or an older release."""

    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="test-gen-rom-types-")
        self.addCleanup(shutil.rmtree, self.dir, True)

    def tree(self, name, version, files):
        c = os.path.join(self.dir, name, "components")
        ld = os.path.join(c, "esp_rom", "esp32s3", "ld")
        os.makedirs(ld)
        os.makedirs(os.path.join(c, "esp_common", "include"))
        major, minor = version
        with open(os.path.join(c, "esp_common", "include", "esp_idf_version.h"), "w") as f:
            f.write(f"#define ESP_IDF_VERSION_MAJOR {major}\n#define ESP_IDF_VERSION_MINOR {minor}\n")
        for f, text in files.items():
            with open(os.path.join(ld, f), "w") as out:
                out.write(text)
        return os.path.join(self.dir, name)

    def test_conflicting_names_are_not_typed(self):
        rom = "".join(f"{rom} = {a:#x};\n" for a, (_, rom) in S3_HOOKS.items())
        bt = "".join(f"{hook} = {a:#x};\n" for a, (hook, _) in S3_HOOKS.items())
        api = "PROVIDE ( esp_rom_crc32_le = crc32_le );\n"
        new = self.tree("v6.1", (6, 1), {"esp32s3.rom.ld": rom, "esp32s3.rom.bt_funcs.ld": bt,
                                         "esp32s3.rom.api.ld": api})
        old = self.tree("v5.4.1", (5, 4), {"esp32s3.rom.ld": rom + "crc32_le_old = 0x40001c98;\n",
                                           "esp32s3.rom.bt_funcs.ld": bt})
        table = {rom: a for a, (_, rom) in S3_HOOKS.items()}
        names = types.rom_names(new, "esp32s3", table, [old])
        for a, (hook, rom) in S3_HOOKS.items():
            self.assertEqual(names[rom], a)
            self.assertNotIn(hook, names)
        # An older release's alias from the script that names the address
        # as the table does, and the API aliases, stay.
        self.assertEqual(names["crc32_le_old"], 0x40001C98)
        self.assertEqual(names["esp_rom_crc32_le"], 0x40001C98)

    def test_conflicting(self):
        scripts = [("esp32s3.rom.ld", [("crc32_le", 0x40001C98), ("crc32_alias", 0x40001C98)]),
                   ("esp32s3.rom.bt_funcs.ld", [("r_lld_con_frm_isr_hook", 0x40001C98), ("r_x", 0x40004000)])]
        self.assertEqual(types.conflicting(scripts, {"crc32_le": 0x40001C98}),
                         {("r_lld_con_frm_isr_hook", 0x40001C98)})
        # A table that names the address with neither script's name settles
        # nothing.
        self.assertEqual(types.conflicting(scripts, {"other": 0x40001C98}), set())


class BundledTables(unittest.TestCase):
    """The bundled ESP32-S3 table, generated with the rule."""

    def test_s3_hooks_are_not_in_the_table(self):
        table = set()
        with open(os.path.join(ROOT, "view", "espressif", "data", "rom_symbols", "esp32s3.txt")) as f:
            for line in f:
                words = line.split()
                if len(words) == 2 and not words[0].startswith("#") and words[0] != "revision":
                    table.add((int(words[0], 16), words[1]))
        for a, (hook, rom) in S3_HOOKS.items():
            self.assertIn((a, rom), table)
            self.assertFalse(any(name == hook for _, name in table), hook)
            self.assertEqual({name for addr, name in table if addr == a}, {rom})


if __name__ == "__main__":
    unittest.main()
