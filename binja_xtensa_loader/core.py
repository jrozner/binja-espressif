"""The loader's logic, independent of Binary Ninja.

Nothing here imports `binaryninja`: `plugin.py` gathers the facts from the
running Binary Ninja (core ABI range, plugin directory, whether the native
plugin is loaded, settings) and passes them in, so this module can be unit
tested with the standard library alone (tests/test_loader.py), and the
release tooling (scripts/release_manifest.py) shares its manifest parser.

Trust model: `manifest.json` is committed next to this file, so it arrives
in the source archive the extension manager installed. A downloaded library
is accepted only if its size and SHA-256 match the manifest entry; hashes
published alongside the release are never consulted.
"""

import hashlib
import json
import os
import re
import sys
import tempfile
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from enum import Enum
from typing import Callable, Iterable, List, Optional, Tuple

MANIFEST_SCHEMA = 1

# Release asset platforms and their library extensions.
PLATFORM_EXTENSIONS = {
    "linux-x86_64": ".so",
    "windows-x86_64": ".dll",
    "macos-universal": ".dylib",
}
LIBRARY_EXTENSIONS = (".so", ".dll", ".dylib")

# binja_xtensa-abi<N>-<platform>.<ext>, as release.yml names the assets.
ASSET_NAME = re.compile(r"^binja_xtensa-abi(?P<abi>[0-9]+)-(?P<platform>[a-z0-9_]+-[a-z0-9_]+)(?P<ext>\.[a-z]+)$")
# X.Y or X.Y.Z, optionally with a semver pre-release suffix (0.2.0-rc.1),
# which release.yml publishes as a GitHub pre-release. No build metadata (+).
VERSION = re.compile(r"^(?P<release>[0-9]+\.[0-9]+(?:\.[0-9]+)?)(?:-(?P<prerelease>[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?\Z")
VERSION_FORMAT = "X.Y or X.Y.Z, optionally followed by -<pre-release> such as -rc.1"
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")

# The library this loader installs into the user plugin directory: one name
# per platform, whatever the version or ABI, so installing a new version
# replaces the old one and two versions are never installed side by side.
INSTALLED_STEM = "binja_xtensa-native"

# Downloads start at github.com and may only be redirected to GitHub's
# release asset storage.
DOWNLOAD_HOST = "github.com"
REDIRECT_HOSTS = frozenset({
    "github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
})
MAX_REDIRECTS = 5
TIMEOUT = 30  # seconds, per connection attempt and per read
LOOPBACK_HOSTS = frozenset({"127.0.0.1", "localhost", "::1"})

# An architecture the native plugin registers: if it exists, the native
# plugin is loaded in this session.
NATIVE_ARCHITECTURE = "xtensa-esp32"

BUILD_FROM_SOURCE = (
    "To use the plugin elsewhere, build it from source: see "
    "https://github.com/jrozner/binja-xtensa#building-from-source"
)


class LoaderError(Exception):
    """A failure the loader reports to the user; never fatal to Binary Ninja."""


class ManifestError(LoaderError):
    pass


class VerificationError(LoaderError):
    pass


class DownloadError(LoaderError):
    pass


# ---------------------------------------------------------------------------
# Manifest


@dataclass(frozen=True)
class Asset:
    name: str
    platform: str
    abi: int
    sha256: str
    size: int


@dataclass(frozen=True)
class Manifest:
    version: str
    repository: str
    assets: Tuple[Asset, ...]
    build: Optional[dict] = field(default=None, compare=False)

    @property
    def tag(self) -> str:
        return "v" + self.version

    def asset_url(self, asset: Asset) -> str:
        return "https://{}/{}/releases/download/{}/{}".format(
            DOWNLOAD_HOST, self.repository, urllib.parse.quote(self.tag), urllib.parse.quote(asset.name))


def _require(cond: bool, message: str) -> None:
    if not cond:
        raise ManifestError(message)


def parse_asset(entry) -> Asset:
    _require(isinstance(entry, dict), "manifest asset is not an object")
    name = entry.get("name")
    _require(isinstance(name, str), "manifest asset has no name")
    m = ASSET_NAME.match(name)
    _require(m is not None, "manifest asset name {!r} is not binja_xtensa-abi<N>-<platform>.<ext>".format(name))
    platform = m.group("platform")
    _require(PLATFORM_EXTENSIONS.get(platform) == m.group("ext"),
             "manifest asset {!r}: unknown platform or wrong extension".format(name))
    abi = int(m.group("abi"))
    sha = entry.get("sha256")
    _require(isinstance(sha, str) and SHA256.match(sha) is not None,
             "manifest asset {!r}: sha256 is not 64 lowercase hex digits".format(name))
    size = entry.get("size")
    _require(isinstance(size, int) and not isinstance(size, bool) and size > 0,
             "manifest asset {!r}: size is not a positive integer".format(name))
    # The redundant fields must agree with the name, which is what's downloaded.
    _require(entry.get("platform", platform) == platform and entry.get("abi", abi) == abi,
             "manifest asset {!r}: platform/abi disagree with its name".format(name))
    return Asset(name=name, platform=platform, abi=abi, sha256=sha, size=size)


def parse_manifest(text) -> Manifest:
    try:
        data = json.loads(text)
    except ValueError as e:
        raise ManifestError("manifest is not valid JSON: {}".format(e))
    _require(isinstance(data, dict), "manifest is not a JSON object")
    _require(data.get("schema") == MANIFEST_SCHEMA, "unsupported manifest schema {!r}".format(data.get("schema")))
    version = data.get("version")
    _require(isinstance(version, str) and VERSION.match(version) is not None,
             "manifest version {!r} is not {}".format(version, VERSION_FORMAT))
    repository = data.get("repository")
    _require(isinstance(repository, str) and REPOSITORY.match(repository) is not None,
             "manifest repository {!r} is not owner/name".format(repository))
    entries = data.get("assets")
    _require(isinstance(entries, list), "manifest assets is not a list")
    assets = tuple(parse_asset(e) for e in entries)
    names = [a.name for a in assets]
    _require(len(set(names)) == len(names), "manifest lists an asset twice")
    keys = [(a.platform, a.abi) for a in assets]
    _require(len(set(keys)) == len(keys), "manifest lists two assets for the same platform and ABI")
    build = data.get("build")
    if build is not None:
        _require(isinstance(build, dict), "manifest build is not an object")
        _require(isinstance(build.get("commit"), str) and COMMIT.match(build["commit"]) is not None,
                 "manifest build.commit is not a full commit hash")
        _require(isinstance(build.get("run_id"), int) and not isinstance(build.get("run_id"), bool),
                 "manifest build.run_id is not an integer")
    _require(not assets or build is not None, "manifest lists assets but no build")
    return Manifest(version=version, repository=repository, assets=assets, build=build)


def is_prerelease(version: str) -> bool:
    """Whether `version` (which must match VERSION) has a pre-release suffix."""
    m = VERSION.match(version)
    if m is None:
        raise ManifestError("version {!r} is not {}".format(version, VERSION_FORMAT))
    return m.group("prerelease") is not None


def load_manifest(path: str) -> Manifest:
    try:
        with open(path, "rb") as f:
            return parse_manifest(f.read())
    except OSError as e:
        raise ManifestError("cannot read {}: {}".format(path, e))


def manifest_json(manifest: Manifest) -> str:
    data = {
        "schema": MANIFEST_SCHEMA,
        "version": manifest.version,
        "repository": manifest.repository,
        "build": manifest.build,
        "assets": [
            {"name": a.name, "platform": a.platform, "abi": a.abi, "sha256": a.sha256, "size": a.size}
            for a in sorted(manifest.assets, key=lambda a: (a.platform, a.abi))
        ],
    }
    return json.dumps(data, indent=2) + "\n"


def read_plugin_version(path: str) -> str:
    try:
        with open(path, "rb") as f:
            version = json.loads(f.read()).get("version")
    except (OSError, ValueError, AttributeError) as e:
        raise ManifestError("cannot read the version from {}: {}".format(path, e))
    if not isinstance(version, str):
        raise ManifestError("{} has no version".format(path))
    return version


# ---------------------------------------------------------------------------
# Choosing the asset


def host_platform(system: Optional[str] = None, machine: Optional[str] = None) -> Optional[str]:
    """The release platform for this OS and CPU, or None if none is built."""
    if system is None:
        system = sys.platform
    if machine is None:
        import platform
        machine = platform.machine()
    machine = machine.lower()
    if system.startswith("linux") and machine in ("x86_64", "amd64"):
        return "linux-x86_64"
    if system in ("win32", "cygwin") and machine in ("amd64", "x86_64"):
        return "windows-x86_64"
    if system == "darwin" and machine in ("arm64", "aarch64", "x86_64"):
        return "macos-universal"
    return None


def select_asset(manifest: Manifest, platform: Optional[str], abi_min: int, abi_max: int) -> Optional[Asset]:
    """The asset for `platform` with the newest core ABI the core accepts.

    A native plugin built against core ABI N (its CorePluginABIVersion) loads
    in a core whose minimum ABI <= N <= current ABI."""
    candidates = [a for a in manifest.assets if a.platform == platform and abi_min <= a.abi <= abi_max]
    return max(candidates, key=lambda a: a.abi, default=None)


def describe_supported(manifest: Manifest) -> str:
    if not manifest.assets:
        return "this version has no prebuilt native libraries"
    return "prebuilt native libraries exist for " + ", ".join(
        "{} (core ABI {})".format(a.platform, a.abi) for a in sorted(manifest.assets, key=lambda a: (a.platform, a.abi)))


def unsupported_message(manifest: Manifest, platform: Optional[str], abi_min: int, abi_max: int) -> str:
    here = platform or "this OS/CPU"
    abis = str(abi_min) if abi_min == abi_max else "{}-{}".format(abi_min, abi_max)
    return ("No prebuilt native library of binja-xtensa {} matches {} with core ABI {}; {}. {}".format(
        manifest.version, here, abis, describe_supported(manifest), BUILD_FROM_SOURCE))


# ---------------------------------------------------------------------------
# State of the plugin directory


def installed_name(platform: str) -> str:
    return INSTALLED_STEM + PLATFORM_EXTENSIONS[platform]


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class State(Enum):
    MISSING = "missing"
    OUTDATED = "outdated"  # present, but not the asset this version selects
    CURRENT = "current"


def installed_state(path: str, asset: Asset) -> State:
    if not os.path.lexists(path):
        return State.MISSING
    try:
        return State.CURRENT if sha256_file(path) == asset.sha256 else State.OUTDATED
    except OSError:
        return State.OUTDATED


def find_other_copies(plugin_dir: str, own_name: str) -> List[str]:
    """Other libraries of this plugin in the plugin directory.

    Matches what building from source produces (libbinja_xtensa.so,
    binja_xtensa.dll, libbinja_xtensa.dylib), release assets downloaded by
    hand (binja_xtensa-abi187-linux-x86_64.so), and links to either."""
    found = []
    try:
        names = os.listdir(plugin_dir)
    except OSError:
        return found
    for name in sorted(names):
        lower = name.lower()
        if name == own_name or lower.startswith("."):
            continue
        if not lower.endswith(LIBRARY_EXTENSIONS):
            continue
        if "binja_xtensa" not in lower and "binja-xtensa" not in lower:
            continue
        path = os.path.join(plugin_dir, name)
        if os.path.isfile(path) or os.path.islink(path):
            found.append(name)
    return found


# ---------------------------------------------------------------------------
# Consent


class Action(Enum):
    NOTHING = "nothing"
    PROMPT = "prompt"  # ask in the UI, then download
    DOWNLOAD = "download"  # the user opted in with the autoDownload setting
    INSTRUCT = "instruct"  # headless without opt-in: log how to install


@dataclass(frozen=True)
class Plan:
    action: Action
    message: str


def plan_startup(*, asset: Optional[Asset], state: Optional[State], other_copies: Iterable[str],
                 native_loaded: bool, ui: bool, auto_download: bool, declined: Optional[str],
                 setting: str, plugin_dir: str) -> Plan:
    """What to do at startup. Nothing is ever downloaded without consent:
    either the autoDownload setting is on, or the user accepts a prompt."""
    other_copies = list(other_copies)
    if asset is None or state is None:
        return Plan(Action.NOTHING, "")
    if other_copies:
        if native_loaded and state is not State.MISSING:
            extra = (" Both are installed, so the architectures are registered twice; remove one of them "
                     "(the downloaded copy with the 'Remove Native Library' command).")
        else:
            extra = ""
        return Plan(Action.NOTHING, (
            "binja-xtensa's native library is already present in {} as {}, so the prebuilt library is not "
            "installed (two copies would register the same architectures twice). To use the prebuilt "
            "library instead, delete it and restart Binary Ninja.{}".format(
                plugin_dir, ", ".join(other_copies), extra)))
    if state is State.CURRENT:
        if native_loaded:
            return Plan(Action.NOTHING, "")
        return Plan(Action.NOTHING, (
            "binja-xtensa's native library is installed but not loaded: restart Binary Ninja to load it. "
            "If it is still not loaded after a restart, the log shows why (for example a core ABI mismatch)."))
    if state is State.MISSING and native_loaded:
        # Loaded from somewhere other than the user plugin directory.
        return Plan(Action.NOTHING, "")
    if auto_download:
        return Plan(Action.DOWNLOAD, "")
    if ui:
        if declined == asset.sha256:
            return Plan(Action.NOTHING, (
                "binja-xtensa's native library is not installed (declined earlier). Install it with "
                "Plugins > Espressif > Install or Update Native Library."))
        return Plan(Action.PROMPT, "")
    verb = "installed" if state is State.MISSING else "up to date"
    return Plan(Action.INSTRUCT, (
        "binja-xtensa's native library is not {}. Nothing is downloaded without consent: enable the "
        "'{}' setting (in settings.json of the user directory, or once in the UI) and start Binary Ninja "
        "again to download {}, or install it from the UI with Plugins > Espressif > Install or Update "
        "Native Library.".format(verb, setting, asset.name)))


def read_declined(path: str) -> Optional[str]:
    try:
        with open(path, "rb") as f:
            value = json.loads(f.read()).get("declined")
        return value if isinstance(value, str) else None
    except (OSError, ValueError, AttributeError):
        return None


def write_declined(path: str, sha256: Optional[str]) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump({"declined": sha256}, f)
    os.replace(tmp, path)


# ---------------------------------------------------------------------------
# Download and install


def _check_url(url: str, test_host: Optional[str]) -> None:
    parts = urllib.parse.urlsplit(url)
    host = (parts.hostname or "").lower()
    if test_host is not None:
        if host == test_host and parts.scheme in ("http", "https"):
            return
        raise DownloadError("refusing to fetch {}: not the local test server".format(url))
    if parts.scheme != "https" or host not in REDIRECT_HOSTS or parts.port not in (None, 443):
        raise DownloadError("refusing to fetch {}: only HTTPS from GitHub is allowed".format(url))


class _RedirectHandler(urllib.request.HTTPRedirectHandler):
    max_redirections = MAX_REDIRECTS

    def __init__(self, test_host: Optional[str]):
        super().__init__()
        self.test_host = test_host

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        _check_url(newurl, self.test_host)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def _test_host(base_url: Optional[str]) -> Optional[str]:
    if base_url is None:
        return None
    host = (urllib.parse.urlsplit(base_url).hostname or "").lower()
    if host not in LOOPBACK_HOSTS:
        raise DownloadError("the test base URL must be a loopback address, not {}".format(base_url))
    return host


def asset_url(manifest: Manifest, asset: Asset, test_base_url: Optional[str] = None) -> str:
    """The download URL: the GitHub release of the manifest's version, or a
    local test server (used by tests/test_headless.py only). Either way the
    download is checked against the committed manifest."""
    if test_base_url is not None:
        _test_host(test_base_url)
        return test_base_url.rstrip("/") + "/" + urllib.parse.quote(asset.name)
    return manifest.asset_url(asset)


def download(url: str, asset: Asset, dest_dir: str, *, test_base_url: Optional[str] = None,
             timeout: float = TIMEOUT, progress: Optional[Callable[[int, int], None]] = None,
             opener=None) -> str:
    """Downloads `url` to a temporary file in `dest_dir` and returns its path
    once its size and SHA-256 match `asset`. Otherwise the file is removed
    and DownloadError or VerificationError is raised."""
    test_host = _test_host(test_base_url)
    _check_url(url, test_host)
    if opener is None:
        opener = urllib.request.build_opener(_RedirectHandler(test_host))
    fd, tmp = tempfile.mkstemp(dir=dest_dir, prefix="." + INSTALLED_STEM + "-", suffix=".part")
    try:
        h = hashlib.sha256()
        received = 0
        with os.fdopen(fd, "wb") as out:
            request = urllib.request.Request(url, headers={"User-Agent": "binja-xtensa-loader"})
            try:
                with opener.open(request, timeout=timeout) as response:
                    _check_url(response.geturl(), test_host)
                    while True:
                        chunk = response.read(1 << 16)
                        if not chunk:
                            break
                        received += len(chunk)
                        if received > asset.size:
                            raise VerificationError("{} is larger than the {} bytes the manifest lists".format(
                                asset.name, asset.size))
                        h.update(chunk)
                        out.write(chunk)
                        if progress is not None:
                            progress(received, asset.size)
            except LoaderError:
                raise
            except Exception as e:  # URLError, HTTPError, timeouts, TLS errors
                if hasattr(e, "close"):
                    e.close()  # an HTTPError holds the response open
                raise DownloadError("downloading {} failed: {}".format(url, e))
            out.flush()
            os.fsync(out.fileno())
        if received != asset.size:
            raise VerificationError("{} is {} bytes, the manifest lists {}".format(asset.name, received, asset.size))
        digest = h.hexdigest()
        if digest != asset.sha256:
            raise VerificationError("{} has SHA-256 {}, but the manifest committed in the plugin lists {}; "
                                    "not installing it".format(asset.name, digest, asset.sha256))
        return tmp
    except BaseException:
        _remove(tmp)
        raise


def _remove(path: str) -> bool:
    try:
        os.remove(path)
        return True
    except FileNotFoundError:
        return True
    except OSError:
        return False


def _set_aside(path: str) -> None:
    """Renames `path` out of the way. Windows refuses to replace or delete a
    loaded DLL but allows renaming it; the renamed file no longer ends in
    .dll, so Binary Ninja ignores it, and cleanup_stale removes it later."""
    for i in range(100):
        aside = "{}.old{}".format(path, i)
        if not os.path.lexists(aside):
            os.rename(path, aside)
            return
    raise OSError("cannot rename {} out of the way".format(path))


def install_file(tmp: str, dest: str) -> None:
    """Atomically moves the verified `tmp` to `dest`, replacing any old
    version (both are in the plugin directory, so this is a rename)."""
    if os.name == "posix":
        os.chmod(tmp, 0o755)
    try:
        os.replace(tmp, dest)
    except PermissionError:
        if not os.path.lexists(dest):
            raise
        _set_aside(dest)
        os.replace(tmp, dest)


def install(manifest: Manifest, asset: Asset, plugin_dir: str, *, test_base_url: Optional[str] = None,
            timeout: float = TIMEOUT, progress: Optional[Callable[[int, int], None]] = None, opener=None) -> str:
    """Downloads, verifies and installs `asset`; returns the installed path."""
    os.makedirs(plugin_dir, exist_ok=True)
    dest = os.path.join(plugin_dir, installed_name(asset.platform))
    tmp = download(asset_url(manifest, asset, test_base_url), asset, plugin_dir, test_base_url=test_base_url,
                   timeout=timeout, progress=progress, opener=opener)
    try:
        install_file(tmp, dest)
    except BaseException:
        _remove(tmp)
        raise
    return dest


def uninstall(plugin_dir: str, platform: str) -> bool:
    """Removes the installed library; returns whether there was one."""
    dest = os.path.join(plugin_dir, installed_name(platform))
    if not os.path.lexists(dest):
        return False
    try:
        os.remove(dest)
    except PermissionError:
        _set_aside(dest)
    return True


def cleanup_stale(plugin_dir: str, now: Optional[float] = None) -> None:
    """Removes leftovers: set-aside old versions, and downloads interrupted
    over an hour ago (a younger one may belong to another running instance)."""
    import time
    now = time.time() if now is None else now
    try:
        names = os.listdir(plugin_dir)
    except OSError:
        return
    for name in names:
        path = os.path.join(plugin_dir, name)
        if name.startswith(INSTALLED_STEM + ".") and ".old" in name[len(INSTALLED_STEM):]:
            _remove(path)
        elif name.startswith("." + INSTALLED_STEM + "-") and name.endswith(".part"):
            try:
                if now - os.path.getmtime(path) > 3600:
                    _remove(path)
            except OSError:
                pass
