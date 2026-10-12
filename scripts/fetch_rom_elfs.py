#!/usr/bin/env python3
"""Downloads Espressif's ROM ELF files for binja-espressif, without Binary Ninja.

Fetches the esp-rom-elfs release the plugin pins (its tag, URL, size and
SHA-256 are read from view/espressif/src/rom_images.rs, the one place they are
kept), checks the archive's size and SHA-256, and unpacks only the ROM ELF
files of the chips the plugin supports, under names made from the chip and
revision, by the same rules as the plugin (nothing in a directory, no links,
no member a pax or GNU long-name header renames). Nothing is written unless
the archive checks out.

By default they go to the plugin's download cache,
<Binary Ninja user directory>/espressif/rom-elfs/<tag>/, where the plugin
finds them by itself. The user directory is $BN_USER_DIRECTORY, else
~/.binaryninja (Linux), ~/Library/Application Support/Binary Ninja (macOS)
or %APPDATA%\\Binary Ninja (Windows). With --dir they go elsewhere: point
the espressif.rom.elfDirectory setting (or load option) at it.

The ELF files are licensed under Apache-2.0; the ROM code is closed source,
Copyright Espressif Systems, with third-party components (see
https://github.com/espressif/esp-rom-elfs).

Usage: scripts/fetch_rom_elfs.py [--dir DIR] [--archive FILE] [--source FILE]

Standard library only.
"""
import argparse
import gzip
import hashlib
import io
import os
import re
import sys
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = os.path.join(ROOT, "view", "espressif", "src", "rom_images.rs")

# The chips the plugin supports (Chip::platform_name in view/espressif/src/lib.rs;
# tests/test_fetch_rom_elfs.py checks they agree).
CHIPS = ("esp8266", "esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c6", "esp32h2", "esp32c5")

# The limits of view/espressif/src/rom_images.rs and rom_download.rs.
MAX_DOWNLOAD = 64 << 20
MAX_ARCHIVE = 64 << 20
MAX_MEMBER = 16 << 20


class Error(Exception):
    pass


def pinned_release(source=SOURCE):
    """The release pinned in rom_images.rs: {tag, url, sha256, size}."""
    with open(source, encoding="utf-8") as f:
        text = f.read()
    m = re.search(r"pub const RELEASE: Release = Release \{(.*?)\};", text, re.S)
    if not m:
        raise Error(f"no RELEASE in {source}")
    body = m.group(1)
    out = {}
    for key in ("tag", "url", "sha256"):
        v = re.search(rf'\b{key}: "([^"]+)"', body)
        if not v:
            raise Error(f"no {key} in {source}'s RELEASE")
        out[key] = v.group(1)
    size = re.search(r"\bsize: ([0-9_]+)", body)
    if not size:
        raise Error(f"no size in {source}'s RELEASE")
    out["size"] = int(size.group(1).replace("_", ""))
    if not out["url"].startswith("https://") or not re.fullmatch(r"[0-9a-fA-F]{64}", out["sha256"]):
        raise Error(f"{source}'s RELEASE is malformed: {out}")
    if not re.fullmatch(r"[A-Za-z0-9._-]+", out["tag"]):
        raise Error(f"{source}'s release tag is not a directory name: {out['tag']!r}")
    return out


def user_directory(env=None, platform=None, home=None):
    """Binary Ninja's user directory."""
    env = os.environ if env is None else env
    platform = sys.platform if platform is None else platform
    home = os.path.expanduser("~") if home is None else home
    if env.get("BN_USER_DIRECTORY"):
        return env["BN_USER_DIRECTORY"]
    if platform == "darwin":
        return os.path.join(home, "Library", "Application Support", "Binary Ninja")
    if platform.startswith("win"):
        appdata = env.get("APPDATA") or os.path.join(home, "AppData", "Roaming")
        return os.path.join(appdata, "Binary Ninja")
    return os.path.join(home, ".binaryninja")


def parse_member_name(name):
    """(chip, revision) of a file named <chip>_rev<REV>_rom.elf for a
    supported chip, else None (rom_images::parse_member_name)."""
    m = re.fullmatch(r"([a-z0-9]+)_rev(0|[1-9][0-9]{0,4})_rom\.elf", name)
    if not m or m.group(1) not in CHIPS:
        return None
    rev = int(m.group(2))
    return (m.group(1), rev) if rev <= 0xFFFF else None


def verify(data, release):
    if len(data) != release["size"]:
        raise Error(f"it is {len(data)} bytes, not the {release['size']} bytes of esp-rom-elfs {release['tag']}")
    sha256 = hashlib.sha256(data).hexdigest()
    if sha256 != release["sha256"].lower():
        raise Error(f"its SHA-256 is {sha256}, not {release['sha256']} (esp-rom-elfs {release['tag']})")


def _octal(field):
    s = field.decode("ascii", "replace").strip("\0 ")
    if not s:
        return 0
    try:
        return int(s, 8)
    except ValueError:
        raise Error("malformed tar header") from None


def tar_files(tar):
    """[(name, data)] of a tar archive's regular files (rom_images::tar_files):
    directories, links, devices and pax/GNU metadata entries are skipped, and
    so is a member whose name such an entry would override."""
    out = []
    at = 0
    renamed = False
    while at + 512 <= len(tar):
        header = tar[at:at + 512]
        if header == bytes(512):
            break
        checksum = _octal(header[148:156])
        if sum(header[:148]) + 8 * 32 + sum(header[156:]) != checksum:
            raise Error(f"bad tar header checksum at offset {at}")
        size = _octal(header[124:136])
        start = at + 512
        end = start + size
        if end > len(tar):
            raise Error("truncated tar member")
        kind = header[156:157]

        def field(b):
            return b.split(b"\0", 1)[0].decode("utf-8", "replace")

        if kind in (b"x", b"L", b"K"):
            renamed = True
        elif kind in (b"0", b"\0"):
            name = field(header[0:100])
            if header[257:262] == b"ustar":
                prefix = field(header[345:500])
                if prefix:
                    name = f"{prefix}/{name}"
            if not renamed:
                out.append((name, tar[start:end]))
            renamed = False
        else:
            renamed = False
        at = start + (size + 511) // 512 * 512
    return out


def select_members(files):
    """[(chip, rev, data)] of the ROM ELF files to take (rom_images::select_members):
    at the top of the archive or under ./, not too large, first one wins."""
    out = []
    for name, data in files:
        if name.startswith("./"):
            name = name[2:]
        parsed = parse_member_name(name)
        if parsed is None or len(data) > MAX_MEMBER or any((c, r) == parsed for c, r, _ in out):
            continue
        out.append((*parsed, data))
    return out


def unpack(archive, release, dest):
    """Verifies the archive and writes its ROM ELF files into `dest`; returns
    the names written."""
    verify(archive, release)
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(archive)) as gz:
            tar = gz.read(MAX_ARCHIVE + 1)
    except (OSError, EOFError) as e:
        raise Error(f"cannot decompress it: {e}") from None
    if len(tar) > MAX_ARCHIVE:
        raise Error("it decompresses to too much data")
    members = select_members(tar_files(tar))
    if not members:
        raise Error("it has no ROM ELF files")
    os.makedirs(dest, exist_ok=True)
    names = []
    for chip, rev, data in members:
        name = f"{chip}_rev{rev}_rom.elf"
        path = os.path.join(dest, name)
        partial = os.path.join(dest, f".{name}.{os.getpid()}")
        with open(partial, "wb") as f:
            f.write(data)
        os.replace(partial, path)
        names.append(name)
    return names


def download(url):
    request = urllib.request.Request(url, headers={"User-Agent": "binja-espressif"})
    with urllib.request.urlopen(request, timeout=60) as response:
        data = response.read(MAX_DOWNLOAD + 1)
    if len(data) > MAX_DOWNLOAD:
        raise Error("the download is too large")
    return data


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--dir", help="where to put the ROM ELF files (default: the plugin's download cache)")
    parser.add_argument("--archive", help="an esp-rom-elfs archive downloaded already, instead of downloading it")
    parser.add_argument("--source", default=SOURCE, help="rom_images.rs, to read the pinned release from")
    args = parser.parse_args(argv)
    try:
        release = pinned_release(args.source)
        dest = args.dir or os.path.join(user_directory(), "espressif", "rom-elfs", release["tag"])
        if args.archive:
            with open(args.archive, "rb") as f:
                archive = f.read(MAX_DOWNLOAD + 1)
        else:
            print(f"downloading esp-rom-elfs {release['tag']} from {release['url']}", file=sys.stderr)
            archive = download(release["url"])
        names = unpack(archive, release, dest)
    except (Error, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    print(f"esp-rom-elfs {release['tag']} (SHA-256 {release['sha256']}): {len(names)} ROM ELF files in {dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
