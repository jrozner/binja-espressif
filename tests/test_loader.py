#!/usr/bin/env python3
"""Unit tests of the Python loader (binja_xtensa_loader/). Standard library
only; no Binary Ninja needed.

Usage: tests/test_loader.py [-v]
"""
import hashlib
import http.server
import json
import os
import shutil
import sys
import tempfile
import threading
import unittest
from unittest import mock

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from binja_xtensa_loader import core  # noqa: E402

LIB = b"\x7fELF fake native library " * 1000


def read(path):
    with open(path, "rb") as f:
        return f.read()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def asset_entry(name, data=LIB):
    return {"name": name, "sha256": sha(data), "size": len(data)}


def manifest_text(assets, version="0.2.0", build=True):
    return json.dumps({
        "schema": 1,
        "version": version,
        "repository": "jrozner/binja-xtensa",
        "build": {"commit": "a" * 40, "run_id": 1234} if build else None,
        "assets": assets,
    })


MANIFEST = core.parse_manifest(manifest_text([
    asset_entry("binja_xtensa-abi187-linux-x86_64.so"),
    asset_entry("binja_xtensa-abi189-linux-x86_64.so", LIB + b"dev"),
    asset_entry("binja_xtensa-abi187-windows-x86_64.dll"),
    asset_entry("binja_xtensa-abi187-macos-universal.dylib"),
]))


class TempDirTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="bn-xtensa-loader-test-")
        self.addCleanup(shutil.rmtree, self.dir, True)


class TestManifest(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(MANIFEST.version, "0.2.0")
        self.assertEqual(MANIFEST.tag, "v0.2.0")
        self.assertEqual(len(MANIFEST.assets), 4)
        a = MANIFEST.assets[0]
        self.assertEqual((a.platform, a.abi, a.size), ("linux-x86_64", 187, len(LIB)))
        self.assertEqual(
            MANIFEST.asset_url(a),
            "https://github.com/jrozner/binja-xtensa/releases/download/v0.2.0/binja_xtensa-abi187-linux-x86_64.so")

    def test_round_trip(self):
        m = core.parse_manifest(core.manifest_json(MANIFEST))
        self.assertEqual(set(m.assets), set(MANIFEST.assets))
        self.assertEqual((m.version, m.repository, m.build), (MANIFEST.version, MANIFEST.repository, MANIFEST.build))

    def test_committed_manifest_parses(self):
        m = core.load_manifest(os.path.join(ROOT, "binja_xtensa_loader", "manifest.json"))
        version = core.read_plugin_version(os.path.join(ROOT, "plugin.json"))
        self.assertEqual(m.version, version, "manifest.json and plugin.json versions differ")
        self.assertEqual(m.repository, "jrozner/binja-xtensa")

    def test_placeholder_without_build(self):
        m = core.parse_manifest(manifest_text([], build=False))
        self.assertEqual(m.assets, ())

    def test_invalid(self):
        good = asset_entry("binja_xtensa-abi187-linux-x86_64.so")
        cases = {
            "not json": "{",
            "wrong schema": json.dumps({"schema": 2}),
            "bad version": manifest_text([good], version="1.x"),
            "unknown platform": manifest_text([asset_entry("binja_xtensa-abi187-linux-arm64.so")]),
            "wrong extension": manifest_text([asset_entry("binja_xtensa-abi187-linux-x86_64.dll")]),
            "bad name": manifest_text([asset_entry("../../evil.so")]),
            "short hash": manifest_text([dict(good, sha256="abc")]),
            "uppercase hash": manifest_text([dict(good, sha256=good["sha256"].upper())]),
            "zero size": manifest_text([dict(good, size=0)]),
            "bool size": manifest_text([dict(good, size=True)]),
            "abi disagrees": manifest_text([dict(good, abi=188)]),
            "duplicate": manifest_text([good, good]),
            "assets without build": manifest_text([good], build=False),
            "bad repository": json.dumps({"schema": 1, "version": "1.0", "repository": "https://evil/x", "assets": []}),
        }
        for what, text in cases.items():
            with self.subTest(what):
                with self.assertRaises(core.ManifestError):
                    core.parse_manifest(text)

    def test_bad_build(self):
        data = json.loads(manifest_text([asset_entry("binja_xtensa-abi187-linux-x86_64.so")]))
        data["build"]["commit"] = "abc"
        with self.assertRaises(core.ManifestError):
            core.parse_manifest(json.dumps(data))


class TestSelection(unittest.TestCase):
    def test_host_platform(self):
        cases = [
            ("linux", "x86_64", "linux-x86_64"),
            ("linux", "AMD64", "linux-x86_64"),
            ("linux", "aarch64", None),
            ("win32", "AMD64", "windows-x86_64"),
            ("win32", "ARM64", None),
            ("darwin", "arm64", "macos-universal"),
            ("darwin", "x86_64", "macos-universal"),
            ("freebsd13", "amd64", None),
        ]
        for system, machine, expected in cases:
            with self.subTest(system=system, machine=machine):
                self.assertEqual(core.host_platform(system, machine), expected)

    def test_select(self):
        def pick(platform, lo, hi):
            a = core.select_asset(MANIFEST, platform, lo, hi)
            return a and a.name

        # 6.0 stable core: ABI 187 only.
        self.assertEqual(pick("linux-x86_64", 187, 187), "binja_xtensa-abi187-linux-x86_64.so")
        # A dev core accepting 188-189 gets the newest it accepts.
        self.assertEqual(pick("linux-x86_64", 188, 189), "binja_xtensa-abi189-linux-x86_64.so")
        self.assertEqual(pick("linux-x86_64", 187, 190), "binja_xtensa-abi189-linux-x86_64.so")
        self.assertEqual(pick("windows-x86_64", 187, 187), "binja_xtensa-abi187-windows-x86_64.dll")
        self.assertEqual(pick("macos-universal", 180, 187), "binja_xtensa-abi187-macos-universal.dylib")
        # Unsupported ABI or platform.
        self.assertIsNone(pick("windows-x86_64", 188, 189))
        self.assertIsNone(pick("linux-x86_64", 190, 191))
        self.assertIsNone(pick(None, 187, 187))
        self.assertIsNone(core.select_asset(core.parse_manifest(manifest_text([], build=False)), "linux-x86_64", 0, 999))

    def test_unsupported_message(self):
        text = core.unsupported_message(MANIFEST, "windows-x86_64", 188, 189)
        self.assertIn("core ABI 188-189", text)
        self.assertIn("windows-x86_64 (core ABI 187)", text)
        self.assertIn("build it from source", text)
        text = core.unsupported_message(core.parse_manifest(manifest_text([], build=False)), None, 187, 187)
        self.assertIn("no prebuilt native libraries", text)


class FakeRelease(http.server.ThreadingHTTPServer):
    """Serves `files` (name -> bytes); `redirects` (name -> URL) answer 302."""


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        name = self.path.lstrip("/")
        if name in self.server.redirects:
            self.send_response(302)
            self.send_header("Location", self.server.redirects[name])
            self.end_headers()
        elif name in self.server.files:
            body = self.server.files[name]
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)

    def log_message(self, *args):
        pass


class TestInstall(TempDirTest):
    def setUp(self):
        super().setUp()
        self.server = FakeRelease(("127.0.0.1", 0), Handler)
        self.server.files = {}
        self.server.redirects = {}
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.base = "http://127.0.0.1:{}".format(self.server.server_address[1])
        self.asset = MANIFEST.assets[0]
        self.dest = os.path.join(self.dir, "binja_xtensa-native.so")

    def install(self, asset=None):
        return core.install(MANIFEST, asset or self.asset, self.dir, test_base_url=self.base, timeout=5)

    def listing(self):
        return sorted(os.listdir(self.dir))

    def test_install(self):
        self.server.files[self.asset.name] = LIB
        seen = []
        path = core.install(MANIFEST, self.asset, self.dir, test_base_url=self.base, timeout=5,
                            progress=lambda done, total: seen.append((done, total)))
        self.assertEqual(path, self.dest)
        self.assertEqual(read(path), LIB)
        self.assertEqual(self.listing(), ["binja_xtensa-native.so"])
        self.assertEqual(seen[-1], (len(LIB), len(LIB)))
        if os.name == "posix":
            self.assertTrue(os.access(path, os.X_OK))
        self.assertEqual(core.installed_state(path, self.asset), core.State.CURRENT)

    def test_hash_mismatch_does_not_install(self):
        evil = bytearray(LIB)
        evil[100] ^= 1
        self.server.files[self.asset.name] = bytes(evil)
        with self.assertRaises(core.VerificationError):
            self.install()
        self.assertEqual(self.listing(), [])

    def test_hash_mismatch_keeps_old_version(self):
        with open(self.dest, "wb") as f:
            f.write(b"old version")
        self.server.files[self.asset.name] = LIB[:-1] + b"!"
        with self.assertRaises(core.VerificationError):
            self.install()
        self.assertEqual(self.listing(), ["binja_xtensa-native.so"])
        self.assertEqual(read(self.dest), b"old version")

    def test_too_large_or_short(self):
        for body in (LIB + b"x" * 100000, LIB[:-1]):
            with self.subTest(len(body)):
                self.server.files[self.asset.name] = body
                with self.assertRaises(core.VerificationError):
                    self.install()
                self.assertEqual(self.listing(), [])

    def test_http_error(self):
        with self.assertRaises(core.DownloadError):
            self.install()
        self.assertEqual(self.listing(), [])

    def test_replaces_old_version(self):
        with open(self.dest, "wb") as f:
            f.write(b"old version")
        self.assertEqual(core.installed_state(self.dest, self.asset), core.State.OUTDATED)
        self.server.files[self.asset.name] = LIB
        self.install()
        self.assertEqual(self.listing(), ["binja_xtensa-native.so"])
        self.assertEqual(read(self.dest), LIB)

    def test_replace_of_locked_file_sets_it_aside(self):
        # Windows refuses to replace a loaded DLL but lets it be renamed.
        with open(self.dest, "wb") as f:
            f.write(b"old version")
        self.server.files[self.asset.name] = LIB
        real_replace = os.replace
        calls = []

        def replace(src, dst):
            calls.append(dst)
            if len(calls) == 1:
                raise PermissionError("in use")
            return real_replace(src, dst)

        with mock.patch("os.replace", replace):
            self.install()
        self.assertEqual(self.listing(), ["binja_xtensa-native.so", "binja_xtensa-native.so.old0"])
        self.assertEqual(read(self.dest), LIB)
        core.cleanup_stale(self.dir)
        self.assertEqual(self.listing(), ["binja_xtensa-native.so"])

    def test_redirect_to_other_host_refused(self):
        self.server.redirects[self.asset.name] = "http://example.com/" + self.asset.name
        with self.assertRaises(core.DownloadError):
            self.install()
        self.assertEqual(self.listing(), [])

    def test_redirect_on_same_host(self):
        self.server.redirects[self.asset.name] = self.base + "/cdn/" + self.asset.name
        self.server.files["cdn/" + self.asset.name] = LIB
        self.install()
        self.assertEqual(read(self.dest), LIB)

    def test_uninstall(self):
        self.assertFalse(core.uninstall(self.dir, "linux-x86_64"))
        open(self.dest, "wb").close()
        self.assertTrue(core.uninstall(self.dir, "linux-x86_64"))
        self.assertEqual(self.listing(), [])

    def test_cleanup_stale(self):
        names = ["binja_xtensa-native.dll.old3", ".binja_xtensa-native-abc.part", ".binja_xtensa-native-new.part",
                 "binja_xtensa-native.so", "libbinja_xtensa.so", "other.so.old0"]
        for name in names:
            open(os.path.join(self.dir, name), "wb").close()
        old = os.path.getmtime(os.path.join(self.dir, "other.so.old0")) - 7200
        os.utime(os.path.join(self.dir, ".binja_xtensa-native-abc.part"), (old, old))
        core.cleanup_stale(self.dir)
        self.assertEqual(self.listing(), [".binja_xtensa-native-new.part", "binja_xtensa-native.so",
                                          "libbinja_xtensa.so", "other.so.old0"])


class TestUrls(unittest.TestCase):
    def test_release_urls(self):
        for url in ["https://github.com/a/b/releases/download/v1/x.so",
                    "https://release-assets.githubusercontent.com/github-production-release-asset/1/2?x=y",
                    "https://objects.githubusercontent.com/x"]:
            core._check_url(url, None)
        for url in ["http://github.com/a/b/releases/download/v1/x.so",
                    "https://github.com:8443/x",
                    "https://evil.example/x",
                    "https://github.com.evil.example/x",
                    "file:///etc/passwd",
                    "ftp://github.com/x"]:
            with self.subTest(url):
                with self.assertRaises(core.DownloadError):
                    core._check_url(url, None)

    def test_test_base_url_must_be_loopback(self):
        with self.assertRaises(core.DownloadError):
            core.asset_url(MANIFEST, MANIFEST.assets[0], "http://evil.example:8000")
        self.assertEqual(core.asset_url(MANIFEST, MANIFEST.assets[0], "http://127.0.0.1:9/rel/"),
                         "http://127.0.0.1:9/rel/binja_xtensa-abi187-linux-x86_64.so")


class TestDuplicates(TempDirTest):
    def test_find_other_copies(self):
        for name in ["libbinja_xtensa.so", "binja_xtensa.dll", "libbinja_xtensa.dylib",
                     "binja_xtensa-abi187-linux-x86_64.so", "binja_xtensa-native.so", "libother.so",
                     "binja_xtensa.so.bak", ".binja_xtensa-native-1.part", "binja_xtensa-native.so.old0"]:
            open(os.path.join(self.dir, name), "wb").close()
        os.mkdir(os.path.join(self.dir, "binja-xtensa.so"))  # a directory, not a library
        if hasattr(os, "symlink"):
            try:
                os.symlink("/nonexistent/libbinja_xtensa.so", os.path.join(self.dir, "Binja_Xtensa_link.so"))
            except OSError:
                pass
        found = core.find_other_copies(self.dir, "binja_xtensa-native.so")
        expected = ["binja_xtensa-abi187-linux-x86_64.so", "binja_xtensa.dll", "libbinja_xtensa.dylib",
                    "libbinja_xtensa.so"]
        if os.path.islink(os.path.join(self.dir, "Binja_Xtensa_link.so")):
            expected.insert(0, "Binja_Xtensa_link.so")
        self.assertEqual(found, expected)

    def test_missing_dir(self):
        self.assertEqual(core.find_other_copies(os.path.join(self.dir, "nope"), "x"), [])


class TestConsent(unittest.TestCase):
    asset = MANIFEST.assets[0]

    def plan(self, **kw):
        args = dict(asset=self.asset, state=core.State.MISSING, other_copies=[], native_loaded=False, ui=True,
                    auto_download=False, declined=None, setting="espressif.loader.autoDownload", plugin_dir="/p")
        args.update(kw)
        return core.plan_startup(**args)

    def test_ui_prompts_when_missing_or_outdated(self):
        self.assertEqual(self.plan().action, core.Action.PROMPT)
        self.assertEqual(self.plan(state=core.State.OUTDATED).action, core.Action.PROMPT)
        self.assertEqual(self.plan(state=core.State.OUTDATED, native_loaded=True).action, core.Action.PROMPT)

    def test_ui_does_not_prompt_again_after_decline(self):
        p = self.plan(declined=self.asset.sha256)
        self.assertEqual(p.action, core.Action.NOTHING)
        self.assertIn("Install or Update Native Library", p.message)
        # A new version asks again.
        self.assertEqual(self.plan(declined="0" * 64).action, core.Action.PROMPT)

    def test_headless_never_downloads_without_setting(self):
        for state in (core.State.MISSING, core.State.OUTDATED):
            p = self.plan(ui=False, state=state)
            self.assertEqual(p.action, core.Action.INSTRUCT)
            self.assertIn("espressif.loader.autoDownload", p.message)
            self.assertIn(self.asset.name, p.message)

    def test_setting_downloads(self):
        self.assertEqual(self.plan(ui=False, auto_download=True).action, core.Action.DOWNLOAD)
        self.assertEqual(self.plan(ui=True, auto_download=True).action, core.Action.DOWNLOAD)
        self.assertEqual(self.plan(ui=True, auto_download=True, declined=self.asset.sha256).action,
                         core.Action.DOWNLOAD)

    def test_nothing_when_current(self):
        for ui in (True, False):
            for auto in (True, False):
                p = self.plan(state=core.State.CURRENT, native_loaded=True, ui=ui, auto_download=auto)
                self.assertEqual(p, core.Plan(core.Action.NOTHING, ""))
        p = self.plan(state=core.State.CURRENT, native_loaded=False, auto_download=True)
        self.assertEqual(p.action, core.Action.NOTHING)
        self.assertIn("restart", p.message)

    def test_nothing_when_loaded_from_elsewhere(self):
        for auto in (True, False):
            self.assertEqual(self.plan(native_loaded=True, auto_download=auto).action, core.Action.NOTHING)

    def test_nothing_with_other_copy(self):
        for state in core.State:
            for auto in (True, False):
                p = self.plan(state=state, other_copies=["libbinja_xtensa.so"], auto_download=auto, ui=False)
                self.assertEqual(p.action, core.Action.NOTHING)
                self.assertIn("libbinja_xtensa.so", p.message)
                self.assertIn("twice", p.message)

    def test_nothing_when_unsupported(self):
        self.assertEqual(self.plan(asset=None, state=None, auto_download=True).action, core.Action.NOTHING)

    def test_declined_file(self):
        d = tempfile.mkdtemp(prefix="bn-xtensa-loader-test-")
        self.addCleanup(shutil.rmtree, d, True)
        path = os.path.join(d, "espressif", "loader.json")
        self.assertIsNone(core.read_declined(path))
        core.write_declined(path, "ab" * 32)
        self.assertEqual(core.read_declined(path), "ab" * 32)
        core.write_declined(path, None)
        self.assertIsNone(core.read_declined(path))
        with open(path, "w") as f:
            f.write("[")
        self.assertIsNone(core.read_declined(path))


class FakeBinaryNinja:
    """Stand-ins for the parts of the binaryninja module plugin.py uses."""

    def __init__(self, user_dir):
        import enum
        import types

        self.user_dir = user_dir
        self.ui = False
        self.answers = []  # message box answers, in order
        self.messages = []  # (text, buttons) shown
        self.settings = {}
        self.loaded = False
        self.actions = []
        self.logs = []
        fake = self

        bn = types.ModuleType("binaryninja")

        class ArchitectureMeta(type):
            def __getitem__(cls, name):
                if fake.loaded and name == "xtensa-esp32":
                    return object()
                raise KeyError(name)

        bn.Architecture = ArchitectureMeta("Architecture", (), {})
        bn.core_ui_enabled = lambda: fake.ui
        bn.user_directory = lambda: user_dir
        bn.user_plugin_path = lambda: os.path.join(user_dir, "plugins")

        bncore = types.ModuleType("binaryninja._binaryninjacore")
        bncore.BNGetCurrentCoreABIVersion = lambda: 187
        bncore.BNGetMinimumCoreABIVersion = lambda: 187

        enums = types.ModuleType("binaryninja.enums")
        enums.MessageBoxButtonResult = enum.IntEnum("MessageBoxButtonResult", "NoButton YesButton OKButton", start=0)
        enums.MessageBoxButtonSet = enum.IntEnum("MessageBoxButtonSet", "OKButtonSet YesNoButtonSet", start=0)
        enums.MessageBoxIcon = enum.IntEnum("MessageBoxIcon", "InformationIcon QuestionIcon WarningIcon ErrorIcon",
                                            start=0)

        log = types.ModuleType("binaryninja.log")

        class Logger:
            def __init__(self, session, name):
                pass

            def __getattr__(self, level):
                return lambda message: fake.logs.append((level, message))

        log.Logger = Logger

        plugin = types.ModuleType("binaryninja.plugin")

        class BackgroundTaskThread:
            def __init__(self, text="", can_cancel=False):
                self.progress = text
                self.cancelled = False

            def start(self):
                self.run()  # synchronously, for the test

        plugin.BackgroundTaskThread = BackgroundTaskThread

        settings = types.ModuleType("binaryninja.settings")

        class Settings:
            def register_group(self, group, title):
                return True

            def register_setting(self, key, properties):
                json.loads(properties)
                fake.settings.setdefault(key, False)
                return True

            def get_bool(self, key):
                return fake.settings[key]

        settings.Settings = Settings

        interaction = types.ModuleType("binaryninja.interaction")

        def show_message_box(title, text, buttons, icon):
            fake.messages.append((text, buttons))
            if buttons == enums.MessageBoxButtonSet.YesNoButtonSet:
                return fake.answers.pop(0)
            return enums.MessageBoxButtonResult.OKButton

        interaction.show_message_box = show_message_box

        mainthread = types.ModuleType("binaryninja.mainthread")
        mainthread.execute_on_main_thread_and_wait = lambda func: func()

        ui = types.ModuleType("binaryninjaui")

        class UIAction:
            def __init__(self, func):
                self.func = func

            @staticmethod
            def registerAction(name):
                pass

        class Handler:
            def bindAction(self, name, action):
                fake.actions.append((name, action.func))

        class UIActionHandler:
            @staticmethod
            def globalActions():
                return Handler()

        class Menu:
            @staticmethod
            def mainMenu(name):
                return types.SimpleNamespace(addAction=lambda name, group: None)

        ui.UIAction, ui.UIActionHandler, ui.Menu = UIAction, UIActionHandler, Menu

        bn.interaction, bn.mainthread, bn._binaryninjacore = interaction, mainthread, bncore
        self.modules = {"binaryninja": bn, "binaryninja._binaryninjacore": bncore, "binaryninja.enums": enums,
                        "binaryninja.log": log, "binaryninja.plugin": plugin, "binaryninja.settings": settings,
                        "binaryninja.interaction": interaction, "binaryninja.mainthread": mainthread,
                        "binaryninjaui": ui}
        self.yes = enums.MessageBoxButtonResult.YesButton
        self.no = enums.MessageBoxButtonResult.NoButton


class TestPluginGlue(TempDirTest):
    """plugin.py's consent handling, with a fake binaryninja module."""

    def setUp(self):
        super().setUp()
        self.bn = FakeBinaryNinja(self.dir)
        patcher = mock.patch.dict(sys.modules, self.bn.modules)
        patcher.start()
        self.addCleanup(patcher.stop)
        sys.modules.pop("binja_xtensa_loader.plugin", None)
        self.addCleanup(sys.modules.pop, "binja_xtensa_loader.plugin", None)
        import binja_xtensa_loader.plugin as plugin
        self.plugin = plugin

        # A release of the running version with this platform's asset.
        self.platform = core.host_platform() or "linux-x86_64"
        self.asset_name = "binja_xtensa-abi187-{}{}".format(self.platform, core.PLATFORM_EXTENSIONS[self.platform])
        manifest = os.path.join(self.dir, "manifest.json")
        with open(manifest, "w") as f:
            f.write(manifest_text([asset_entry(self.asset_name)]))
        plugin_json = os.path.join(self.dir, "plugin.json")
        with open(plugin_json, "w") as f:
            json.dump({"version": "0.2.0"}, f)
        for name, value in [("MANIFEST", manifest), ("PLUGIN_JSON", plugin_json)]:
            p = mock.patch.object(plugin, name, value)
            p.start()
            self.addCleanup(p.stop)
        p = mock.patch.object(core, "host_platform", lambda: self.platform)
        p.start()
        self.addCleanup(p.stop)

        self.server = FakeRelease(("127.0.0.1", 0), Handler)
        self.server.files = {self.asset_name: LIB}
        self.server.redirects = {}
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        p = mock.patch.dict(os.environ, {plugin.TEST_BASE_URL_ENV: "http://127.0.0.1:{}".format(
            self.server.server_address[1])})
        p.start()
        self.addCleanup(p.stop)
        self.plugins = os.path.join(self.dir, "plugins")
        os.makedirs(self.plugins)
        self.installed = os.path.join(self.plugins, core.installed_name(self.platform))
        self.plugin.register_settings()

    def prompts(self):
        return [t for t, b in self.bn.messages if b == self.bn.modules["binaryninja.enums"].MessageBoxButtonSet.YesNoButtonSet]

    def test_ui_prompt_accepted_installs(self):
        self.bn.ui = True
        self.bn.answers = [self.bn.yes]
        self.plugin.startup_check()
        self.assertEqual(len(self.prompts()), 1)
        self.assertIn(self.asset_name, self.prompts()[0])
        self.assertEqual(read(self.installed), LIB)
        self.assertIn("Restart Binary Ninja", self.bn.messages[-1][0])

    def test_ui_prompt_declined_is_remembered(self):
        self.bn.ui = True
        self.bn.answers = [self.bn.no]
        self.plugin.startup_check()
        self.assertFalse(os.path.exists(self.installed))
        self.plugin.startup_check()  # no second prompt (answers is empty: a prompt would fail)
        self.assertEqual(len(self.prompts()), 1)
        self.assertFalse(os.path.exists(self.installed))
        # The command still installs: invoking it is the consent.
        self.plugin.install_command()
        self.assertEqual(read(self.installed), LIB)

    def test_headless_without_setting_does_nothing(self):
        self.plugin.startup_check()
        self.assertEqual(self.bn.messages, [])
        self.assertFalse(os.path.exists(self.installed))
        self.assertTrue(any("espressif.loader.autoDownload" in m for _, m in self.bn.logs))

    def test_headless_with_setting_installs(self):
        self.bn.settings[self.plugin.AUTO_DOWNLOAD] = True
        self.plugin.startup_check()
        self.assertEqual(read(self.installed), LIB)

    def test_no_prompt_when_loaded_from_elsewhere(self):
        self.bn.ui = True
        self.bn.loaded = True
        self.plugin.startup_check()
        self.assertEqual(self.bn.messages, [])

    def test_version_mismatch_does_nothing(self):
        with open(self.plugin.PLUGIN_JSON, "w") as f:
            json.dump({"version": "0.3.0"}, f)
        self.bn.settings[self.plugin.AUTO_DOWNLOAD] = True
        self.plugin.startup_check()
        self.assertFalse(os.path.exists(self.installed))
        self.assertTrue(any("not a release checkout" in m for _, m in self.bn.logs))

    def test_start_in_ui_registers_actions_and_remove(self):
        self.bn.ui = True
        self.bn.answers = [self.bn.yes, self.bn.yes]
        self.plugin.start()
        self.assertEqual([n for n, _ in self.bn.actions], [self.plugin.INSTALL_ACTION, self.plugin.REMOVE_ACTION])
        self.assertTrue(os.path.exists(self.installed))
        self.plugin.remove_command()
        self.assertFalse(os.path.exists(self.installed))
        # Removing it declines this version, so the next start doesn't ask.
        self.plugin.startup_check()
        self.assertEqual(len(self.prompts()), 2)

    def test_install_command_refuses_second_copy(self):
        self.bn.ui = True
        open(os.path.join(self.plugins, "libbinja_xtensa" + core.PLATFORM_EXTENSIONS[self.platform]), "wb").close()
        self.plugin.install_command()
        self.assertFalse(os.path.exists(self.installed))
        self.assertIn("twice", self.bn.messages[-1][0])


if __name__ == "__main__":
    unittest.main()
