#!/usr/bin/env python3
"""Unit tests of scripts/fetch_rom_elfs.py, with small synthetic archives.
Standard library only; no Binary Ninja or network needed.

Usage: tests/test_fetch_rom_elfs.py [-v]
"""
import contextlib
import gzip
import hashlib
import io
import os
import re
import shutil
import sys
import tempfile
import unittest
from unittest import mock

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "scripts"))

import fetch_rom_elfs as fetch  # noqa: E402


def read(path):
    with open(path, "rb") as f:
        return f.read()


def tar_entry(name, typeflag, data):
    """A ustar header and its data, as rom_images.rs's tests build them."""
    h = bytearray(512)
    h[:len(name)] = name.encode()
    h[100:108] = b"0000644\0"
    h[124:136] = f"{len(data):011o}\0".encode()
    h[156] = typeflag
    h[257:263] = b"ustar\0"
    h[263:265] = b"00"
    h[148:156] = b" " * 8
    h[148:156] = f"{sum(h):06o}\0 ".encode()
    out = bytes(h) + data
    return out + bytes(-len(out) % 512)


def tar(entries):
    return b"".join(entries) + bytes(1024)


def release_of(archive):
    return {"tag": "test", "url": "https://example.invalid/a.tar.gz",
            "sha256": hashlib.sha256(archive).hexdigest(), "size": len(archive)}


class Tests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="fetch-rom-elfs-test-")
        self.addCleanup(shutil.rmtree, self.dir)

    def test_pinned_release_is_read_from_the_rust_source(self):
        release = fetch.pinned_release()
        self.assertRegex(release["sha256"], r"^[0-9a-f]{64}$")
        self.assertTrue(release["url"].startswith("https://github.com/espressif/esp-rom-elfs/"))
        self.assertIn(release["tag"], release["url"])
        self.assertGreater(release["size"], 1 << 20)
        # The same values tests/test_headless.py reads.
        src = read(fetch.SOURCE).decode()
        self.assertEqual(src.count(release["sha256"]), 1)

    def test_malformed_source(self):
        path = os.path.join(self.dir, "rom_images.rs")
        for text in ("", 'pub const RELEASE: Release = Release { tag: "x", };',
                     'pub const RELEASE: Release = Release { tag: "../x", url: "https://a", '
                     'sha256: "' + "a" * 64 + '", size: 1, };',
                     'pub const RELEASE: Release = Release { tag: "x", url: "http://a", '
                     'sha256: "' + "a" * 64 + '", size: 1, };'):
            with open(path, "w") as f:
                f.write(text)
            with self.assertRaises(fetch.Error, msg=text):
                fetch.pinned_release(path)

    def test_chips_match_the_plugin(self):
        src = read(os.path.join(ROOT, "view", "espressif", "src", "lib.rs")).decode()
        body = src[src.index("pub fn platform_name"):]
        body = body[:body.index("\n    }\n")]
        self.assertEqual(sorted(re.findall(r'=> "(\w+)"', body)), sorted(fetch.CHIPS))

    def test_user_directory(self):
        home = os.path.join(os.sep, "home", "me")
        self.assertEqual(fetch.user_directory({"BN_USER_DIRECTORY": "/bn"}, "linux", home), "/bn")
        self.assertEqual(fetch.user_directory({"BN_USER_DIRECTORY": ""}, "linux", home),
                         os.path.join(home, ".binaryninja"))
        self.assertEqual(fetch.user_directory({}, "darwin", home),
                         os.path.join(home, "Library", "Application Support", "Binary Ninja"))
        self.assertEqual(fetch.user_directory({"APPDATA": "C:\\Users\\me\\AppData\\Roaming"}, "win32", home),
                         os.path.join("C:\\Users\\me\\AppData\\Roaming", "Binary Ninja"))
        self.assertEqual(fetch.user_directory({"BN_USER_DIRECTORY": "D:\\bn"}, "win32", home), "D:\\bn")

    def test_member_names(self):
        self.assertEqual(fetch.parse_member_name("esp32c3_rev101_rom.elf"), ("esp32c3", 101))
        self.assertEqual(fetch.parse_member_name("esp32_rev0_rom.elf"), ("esp32", 0))
        for name in ("esp32c61_rev100_rom.elf", "esp32p4_rev0_rom.elf", "esp32_rev_rom.elf",
                     "esp32_rev00_rom.elf", "esp32_revx_rom.elf", "esp32_rev0_rom.elf.bak",
                     "../esp32_rev0_rom.elf", "dir/esp32_rev0_rom.elf", "esp32_rev999999_rom.elf",
                     "esp32_rev65536_rom.elf", "esp32_rev1_rev0_rom.elf"):
            self.assertIsNone(fetch.parse_member_name(name), name)

    def test_member_filtering(self):
        archive = tar([
            tar_entry("esp32_rev0_rom.elf", ord("0"), b"esp32"),
            tar_entry("./esp32s3_rev0_rom.elf", ord("0"), b"s3"),
            tar_entry("../esp32c3_rev3_rom.elf", ord("0"), b"traversal"),
            tar_entry("/tmp/esp32c3_rev3_rom.elf", ord("0"), b"absolute"),
            tar_entry("sub/esp32c6_rev0_rom.elf", ord("0"), b"nested"),
            tar_entry("esp32h2_rev0_rom.elf", ord("2"), b""),  # a symlink
            tar_entry("esp32c2_rev100_rom.elf", ord("5"), b""),  # a directory
            tar_entry("esp32p4_rev0_rom.elf", ord("0"), b"p4"),
            tar_entry("README.md", ord("0"), b"readme"),
            # A pax header that renames the next member: not trusted.
            tar_entry("PaxHeader", ord("x"), b"30 path=../../esp32_rev300.elf\n"),
            tar_entry("esp32_rev300_rom.elf", ord("0"), b"renamed"),
            tar_entry("esp32c5_rev0_rom.elf", 0, b"old-style"),
            tar_entry("esp32_rev0_rom.elf", ord("0"), b"second"),  # the first one wins
        ])
        files = fetch.tar_files(archive)
        names = [n for n, _ in files]
        self.assertNotIn("esp32h2_rev0_rom.elf", names)
        self.assertNotIn("esp32_rev300_rom.elf", names)
        self.assertEqual(fetch.select_members(files),
                         [("esp32", 0, b"esp32"), ("esp32s3", 0, b"s3"), ("esp32c5", 0, b"old-style")])

    def test_malformed_tar(self):
        archive = bytearray(tar([tar_entry("esp32_rev0_rom.elf", ord("0"), b"esp32")]))
        archive[0] ^= 1
        with self.assertRaises(fetch.Error):
            fetch.tar_files(bytes(archive))
        archive = tar([tar_entry("esp32_rev0_rom.elf", ord("0"), bytes(600))])
        with self.assertRaises(fetch.Error):
            fetch.tar_files(archive[:512 + 100])
        self.assertEqual(fetch.tar_files(b""), [])

    def gz(self, entries):
        return gzip.compress(tar(entries))

    def test_unpacks_only_verified_archives(self):
        archive = self.gz([tar_entry("esp32_rev0_rom.elf", ord("0"), b"esp32"),
                           tar_entry("../esp32_rev300_rom.elf", ord("0"), b"escape")])
        dest = os.path.join(self.dir, "out", "cache")
        self.assertEqual(fetch.unpack(archive, release_of(archive), dest), ["esp32_rev0_rom.elf"])
        self.assertEqual(read(os.path.join(dest, "esp32_rev0_rom.elf")), b"esp32")
        self.assertEqual(sorted(os.listdir(dest)), ["esp32_rev0_rom.elf"])
        self.assertFalse(os.path.exists(os.path.join(self.dir, "out", "esp32_rev300_rom.elf")))
        # A corrupted archive, or the wrong size: nothing is written.
        bad = bytearray(archive)
        bad[len(bad) // 2] ^= 0xFF
        for data, release, what in ((bytes(bad), release_of(archive), "SHA-256"),
                                    (archive + b"x", release_of(archive), "bytes")):
            with self.assertRaisesRegex(fetch.Error, what):
                fetch.unpack(data, release, os.path.join(self.dir, "bad"))
        self.assertFalse(os.path.exists(os.path.join(self.dir, "bad")))
        # A verified archive without ROM ELF files, or that is not gzip.
        empty = self.gz([tar_entry("README.md", ord("0"), b"readme")])
        with self.assertRaisesRegex(fetch.Error, "no ROM ELF"):
            fetch.unpack(empty, release_of(empty), os.path.join(self.dir, "bad"))
        with self.assertRaisesRegex(fetch.Error, "decompress"):
            fetch.unpack(b"not gzip", release_of(b"not gzip"), os.path.join(self.dir, "bad"))
        self.assertFalse(os.path.exists(os.path.join(self.dir, "bad")))

    def run_main(self, args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = fetch.main(args)
        return code, out.getvalue(), err.getvalue()

    def test_main_with_an_archive(self):
        archive = self.gz([tar_entry("esp32c3_rev3_rom.elf", ord("0"), b"c3")])
        path = os.path.join(self.dir, "a.tar.gz")
        with open(path, "wb") as f:
            f.write(archive)
        release = release_of(archive)
        source = os.path.join(self.dir, "rom_images.rs")
        with open(source, "w") as f:
            f.write(f'pub const RELEASE: Release = Release {{\n    tag: "{release["tag"]}",\n'
                    f'    url: "{release["url"]}",\n    sha256: "{release["sha256"].upper()}",\n'
                    f'    size: {release["size"]:_},\n}};\n')
        # --dir
        dest = os.path.join(self.dir, "roms")
        code, out, _ = self.run_main(["--archive", path, "--source", source, "--dir", dest])
        self.assertEqual(code, 0)
        self.assertIn("1 ROM ELF files", out)
        self.assertEqual(os.listdir(dest), ["esp32c3_rev3_rom.elf"])
        # The default: the user directory's download cache.
        user = os.path.join(self.dir, "bn-user")
        with mock.patch.dict(os.environ, {"BN_USER_DIRECTORY": user}):
            code, _, _ = self.run_main(["--archive", path, "--source", source])
        self.assertEqual(code, 0)
        self.assertEqual(os.listdir(os.path.join(user, "espressif", "rom-elfs", "test")), ["esp32c3_rev3_rom.elf"])
        # Another archive: an error, nothing written.
        with open(path, "ab") as f:
            f.write(b"x")
        code, _, err = self.run_main(["--archive", path, "--source", source, "--dir", os.path.join(self.dir, "x")])
        self.assertEqual(code, 1)
        self.assertIn("error:", err)
        self.assertFalse(os.path.exists(os.path.join(self.dir, "x")))

    def test_download_is_capped(self):
        class Response(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

        with mock.patch.object(fetch.urllib.request, "urlopen",
                               return_value=Response(bytes(fetch.MAX_DOWNLOAD + 1))) as urlopen:
            with self.assertRaisesRegex(fetch.Error, "too large"):
                fetch.download("https://example.invalid/a.tar.gz")
        request = urlopen.call_args[0][0]
        self.assertEqual(request.get_header("User-agent"), "binja-espressif")


if __name__ == "__main__":
    unittest.main()
