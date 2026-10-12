#!/usr/bin/env python3
"""End-to-end test of the Python loader in headless Binary Ninja.

Installs the loader (this repository's __init__.py, plugin.json and
binja_espressif_loader/) into a throwaway Binary Ninja user directory, with a
manifest listing a locally built library as the release asset for the
running core's ABI, served by a local HTTP server standing in for the
GitHub release. Then, each in a fresh Binary Ninja process:

1. without the autoDownload setting: nothing is downloaded;
2. with it: the library is downloaded, verified and installed into the
   user plugin directory as binja_espressif-native.so;
3. after the "restart": the native plugin is loaded (its architectures
   exist) and nothing is downloaded again;
4. with a tampered asset on the server and the installed library removed:
   nothing is installed;
5. with a self-built copy in the plugin directory: nothing is installed.

Usage: tests/test_loader_headless.py [path/to/libbinja_espressif.so]

Linux only (the asset is the linux-x86_64 one). Runs Binary Ninja from
~/binaryninja (or $BN_INSTALL_DIR); needs a headless-capable license. The
user directory (bn-espressif-loader-test-* in the temp directory) is removed at
exit unless $BN_ESPRESSIF_KEEP_TMP is set.
"""
import atexit
import hashlib
import http.server
import json
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import threading

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
library = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "release", "libbinja_espressif.so"))
install = os.environ.get("BN_INSTALL_DIR", os.path.expanduser("~/binaryninja"))

user_dir = tempfile.mkdtemp(prefix="bn-espressif-loader-test-")


def remove_user_dir():
    if os.environ.get("BN_ESPRESSIF_KEEP_TMP"):
        print("keeping " + user_dir)
    else:
        shutil.rmtree(user_dir, ignore_errors=True)


atexit.register(remove_user_dir)

plugins = os.path.join(user_dir, "plugins")
loader_dir = os.path.join(plugins, "binja-espressif")
release_dir = os.path.join(user_dir, "release")
installed = os.path.join(plugins, "binja_espressif-native.so")
os.makedirs(release_dir)
os.makedirs(loader_dir)
shutil.copy(os.path.join(ROOT, "__init__.py"), loader_dir)
shutil.copy(os.path.join(ROOT, "plugin.json"), loader_dir)
shutil.copytree(os.path.join(ROOT, "binja_espressif_loader"), os.path.join(loader_dir, "binja_espressif_loader"),
                ignore=shutil.ignore_patterns("__pycache__"))
license_file = os.path.expanduser("~/.binaryninja/license.dat")
if os.path.exists(license_file):
    os.symlink(license_file, os.path.join(user_dir, "license.dat"))

failures = []


def check(cond, what):
    print(("  ok   " if cond else "  FAIL ") + what)
    if not cond:
        failures.append(what)


def write_settings(settings):
    with open(os.path.join(user_dir, "settings.json"), "w") as f:
        json.dump(settings, f)


# A child process runs Binary Ninja: plugins load once per process, and the
# native library only loads at the next start after it is installed.
CHILD = textwrap.dedent("""
    import json, sys
    sys.path.insert(0, sys.argv[1])
    import binaryninja
    from binaryninja import _binaryninjacore as core
    from binaryninja.enums import LogLevel
    if sys.argv[2] == "abi":
        print(json.dumps([core.BNGetMinimumCoreABIVersion(), core.BNGetCurrentCoreABIVersion()]))
        sys.exit(0)
    binaryninja.log_to_stdout(LogLevel.InfoLog)
    binaryninja._init_plugins()
    try:
        binaryninja.Architecture["xtensa-esp32"]
        loaded = True
    except KeyError:
        loaded = False
    print("RESULT " + json.dumps({"loaded": loaded}))
""")


def run_bn(phase):
    env = dict(os.environ, BN_USER_DIRECTORY=user_dir, BN_ESPRESSIF_LOADER_TEST_BASE_URL=base_url)
    env.pop("BN_DISABLE_USER_PLUGINS", None)
    p = subprocess.run([sys.executable, "-c", CHILD, os.path.join(install, "python"), phase], env=env,
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=300)
    print(textwrap.indent(p.stdout.rstrip(), "    | "))
    if p.returncode != 0:
        raise SystemExit("Binary Ninja child process failed in phase {}".format(phase))
    if phase == "abi":
        return json.loads(p.stdout.strip().splitlines()[-1])
    line = next(l for l in p.stdout.splitlines() if l.startswith("RESULT "))
    return json.loads(line[len("RESULT "):]), p.stdout


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=release_dir, **kwargs)

    def do_GET(self):
        requests.append(self.path)
        super().do_GET()

    def log_message(self, *args):
        pass


requests = []
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
base_url = "http://127.0.0.1:{}/".format(server.server_address[1])


def sha256(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


# The release: the local library as the asset for the running core's ABI.
write_settings({})
abi_min, abi_max = run_bn("abi")
asset = "binja_espressif-abi{}-linux-x86_64.so".format(abi_max)
shutil.copy(library, os.path.join(release_dir, asset))
with open(os.path.join(ROOT, "plugin.json")) as f:
    version = json.load(f)["version"]
manifest = {
    "schema": 1,
    "version": version,
    "repository": "jrozner/binja-espressif",
    "build": {"commit": "0" * 40, "run_id": 1},
    "assets": [{"name": asset, "platform": "linux-x86_64", "abi": abi_max,
                "sha256": sha256(library), "size": os.path.getsize(library)}],
}
with open(os.path.join(loader_dir, "binja_espressif_loader", "manifest.json"), "w") as f:
    json.dump(manifest, f)
print("core ABI {}-{}, asset {}".format(abi_min, abi_max, asset))

print("1. without consent")
result, log = run_bn("no-consent")
check(not os.path.lexists(installed), "nothing installed")
check(requests == [], "nothing requested from the server")
check("espressif.loader.autoDownload" in log, "the log says how to consent")
check(not result["loaded"], "native plugin not loaded")

print("2. with espressif.loader.autoDownload")
write_settings({"espressif.loader.autoDownload": True})
result, log = run_bn("download")
check(os.path.isfile(installed), "binja_espressif-native.so installed in the user plugin directory")
check(os.path.isfile(installed) and sha256(installed) == sha256(library), "installed library is the asset")
check(requests == ["/" + asset], "asset requested once")
check(sorted(os.listdir(plugins)) == ["binja-espressif", "binja_espressif-native.so"], "no other files in the plugin directory")
check("Restart Binary Ninja" in log, "the log asks for a restart")

print("3. after restart")
result, log = run_bn("restart")
check(result["loaded"], "native plugin loaded (xtensa-esp32 exists)")
check(requests == ["/" + asset], "nothing downloaded again")

print("4. tampered asset")
os.remove(installed)
with open(os.path.join(release_dir, asset), "r+b") as f:
    f.seek(0x1000)
    byte = f.read(1)
    f.seek(0x1000)
    f.write(bytes([byte[0] ^ 1]))
result, log = run_bn("tampered")
check(not os.path.lexists(installed), "tampered asset not installed")
check("SHA-256" in log, "the log reports the hash mismatch")
check(not [n for n in os.listdir(plugins) if n.endswith(".part")], "no partial download left behind")
shutil.copy(library, os.path.join(release_dir, asset))

print("5. self-built copy present")
os.symlink(library, os.path.join(plugins, "libbinja_espressif.so"))
requests.clear()
result, log = run_bn("self-built")
check(not os.path.lexists(installed), "no second copy installed")
check(requests == [], "nothing requested from the server")
check(result["loaded"], "self-built copy loaded")

server.shutdown()
if failures:
    print("{} failure(s)".format(len(failures)))
    sys.exit(1)
print("all passed")
