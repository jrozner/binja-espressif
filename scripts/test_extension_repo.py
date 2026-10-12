#!/usr/bin/env python3
"""Writes a static one-plugin extension repository for a release, to test
what the extension manager installs before the plugin is listed on
extensions.binary.ninja. See "Testing a release privately" in docs/releasing.md.

  scripts/test_extension_repo.py v0.1.0-rc.1 --github OWNER/REPO[:BRANCH] [-o DIR]
  scripts/test_extension_repo.py v0.1.0-rc.1 --base-url URL [-o DIR]

Binary Ninja's extension manager (the "V2" one) reads third-party
repositories in the format of the extensions.binary.ninja API, which
Binary Ninja's examples/python/mock_extension_server.py also serves. (The
plugins.json list of Vector35/community-plugins is the format of older
versions; current ones reject it.) This writes that API's documents for
one plugin as static files:

  DIR/v2/manifests      the repository's manifests, which Binary Ninja
                        fetches from <URL>/v2/manifests for each URL in the
                        extensionManager.unofficialUrls setting
  DIR/extensions.json   the manifest's extensions: this plugin
  DIR/versions.json     its one version, downloaded from the tag's source
                        archive on GitHub,
                        https://github.com/<repo>/archive/refs/tags/<tag>.zip

BASE_URL is where DIR will be served from (the URLs in the files are
absolute), such as http://127.0.0.1:8000 for `python3 -m http.server 8000`
in DIR; it is also the URL to add to extensionManager.unofficialUrls.
RepositoryManager().add_repository() takes BASE_URL/extensions.json instead.
--github OWNER/REPO[:BRANCH] is short for the base URL of DIR committed at
the root of that branch (default main) of a public GitHub repository,
https://raw.githubusercontent.com/OWNER/REPO/BRANCH. (Binary Ninja fetches
without credentials, so a private repository does not work.)

The record is built from plugin.json as of TAG (`git show TAG:plugin.json`;
--plugin-json reads a file instead), the way the extension server builds it.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import urllib.parse
import uuid
from datetime import datetime, timezone

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from binja_espressif_loader import core  # noqa: E402

DEFAULT_REPOSITORY = "jrozner/binja-espressif"
MANIFESTS = "v2/manifests"
EXTENSIONS = "extensions.json"
VERSIONS = "versions.json"
# The repository's name, which is also the folder Binary Ninja keeps its
# plugins in (channels/<name>/): that of jrozner/binja-extensions-test.
MANIFEST_NAME = "binja-extensions-test"
# Download platforms of the extension server, by plugin.json platform.
PLATFORMS = {
    "Darwin": ("darwin-x64", "darwin-arm64"),
    "Linux": ("linux-x64", "linux-arm64"),
    "Windows": ("win-x64", "win-arm64"),
}
# The extension server's category IDs for plugin.json types.
CATEGORIES = {"analysis": 1, "architecture": 2, "binaryview": 3, "core": 4, "helper": 5, "platform": 7,
              "sync": 8, "ui": 9}
PYTHON_EXTENSION = 0  # the extension server's `type` of a Python plugin
# Column sizes of the extension server (community-plugins' generate_plugininfo.py).
MAX_LENGTHS = {"name": 64, "description": 256, "author": 128, "version": 16}


def package_url(repository, tag):
    return "https://github.com/{}/archive/refs/tags/{}.zip".format(repository, urllib.parse.quote(tag))


def github_base_url(spec):
    """The raw.githubusercontent.com URL of the root of OWNER/REPO[:BRANCH]."""
    repo, colon, branch = spec.partition(":")
    if core.REPOSITORY.match(repo) is None:
        raise ValueError("--github {!r} is not owner/name[:branch]".format(spec))
    if not colon:
        branch = "main"
    if not valid_branch(branch):
        raise ValueError("--github {!r} has an invalid branch name".format(spec))
    return "https://raw.githubusercontent.com/{}/{}".format(
        repo, "/".join(urllib.parse.quote(part, safe="") for part in branch.split("/")))


def valid_branch(name):
    """Whether `name` is a valid branch name (git check-ref-format --branch)."""
    if not name or name == "@" or name.startswith("-") or name.endswith((".", "/")):
        return False
    if any(s in name for s in ("..", "@{", "//")) or any(ord(c) < 0x20 or c in " ~^:?*[\\\x7f" for c in name):
        return False
    return all(part and not part.startswith(".") and not part.endswith(".lock") for part in name.split("/"))


def install_path(repository):
    """The plugin's folder name, as the extension server derives it from
    the GitHub repository (binja-GoReSym of xusheng6 is xusheng6_binjaGoReSym)."""
    return re.sub("[^a-zA-Z0-9_]", "", repository.replace("/", "_"))


def _uuid(*parts):
    return str(uuid.UUID(hashlib.md5(":".join(parts).encode()).hexdigest()))


def _page(results):
    return {"count": len(results), "next": None, "previous": None, "results": results}


def repository(plugin, repo, tag, base_url, url, created):
    """The documents for `plugin` (plugin.json's contents) released as
    `tag`, by file name; `base_url` ends with a slash."""
    for key in ("name", "author", "description", "version", "license", "api", "minimumbinaryninjaversion"):
        if key not in plugin:
            raise ValueError("plugin.json has no {}".format(key))
    if "v" + plugin["version"] != tag:
        raise ValueError("plugin.json at {} is version {}, not {}".format(tag, plugin["version"], tag[1:]))
    for key, limit in MAX_LENGTHS.items():
        if len(plugin[key]) > limit:
            raise ValueError("plugin.json {} is {} characters, the extension server allows {}".format(
                key, len(plugin[key]), limit))
    platforms = [p for name in plugin.get("platforms", sorted(PLATFORMS)) for p in PLATFORMS.get(name, ())]
    if not platforms:
        raise ValueError("plugin.json lists no platform the extension manager knows")
    extension_id = _uuid("extension", repo)
    version_id = _uuid("version", repo, plugin["version"])
    extension = {
        "id": extension_id,
        "name": plugin["name"],
        "type": PYTHON_EXTENSION,
        "short_description": plugin["description"],
        "author_name": plugin["author"],
        "categories": [{"id": CATEGORIES.get(t, 0), "name": t} for t in plugin.get("type", [])],
        "homepage": "https://github.com/" + repo,
        "path": install_path(repo),
        "using_apis": plugin["api"],
        "state": 0,
        "versions_url": base_url + VERSIONS,
        "official": False,
        "is_paid": False,
        "latest_version_id": version_id,
        "view_only": False,
        "license_name": plugin["license"]["name"],
        "license": plugin["license"]["text"],
    }
    version = {
        "id": version_id,
        "version_string": plugin["version"],
        "long_description": plugin.get("longdescription", ""),
        "changelog": "",
        # From requirements.txt on the server; there is none.
        "dependencies": {},
        "minimum_client_version": plugin["minimumbinaryninjaversion"],
        "created": datetime.fromtimestamp(created, timezone.utc).isoformat(),
        "platform_versions": [
            {"platform": {"name": p}, "download_url": url, "untracked_download_url": url} for p in platforms],
        "subdir": "",
    }
    manifest = {"id": _uuid("manifest", repo), "name": MANIFEST_NAME, "extensions_url": base_url + EXTENSIONS}
    return {MANIFESTS: [manifest], EXTENSIONS: _page([extension]), VERSIONS: _page([version])}


def fail(message):
    print("error: " + message, file=sys.stderr)
    sys.exit(1)


def read_plugin_json(path, tag):
    try:
        if path is not None:
            with open(path, "rb") as f:
                return json.loads(f.read())
        text = subprocess.run(["git", "show", "{}:plugin.json".format(tag)], cwd=ROOT, check=True,
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL).stdout
        return json.loads(text)
    except (OSError, ValueError) as e:
        fail("cannot read plugin.json: {}".format(e))
    except subprocess.CalledProcessError:
        fail("cannot read plugin.json at tag {}: fetch the tag (git fetch --tags) or pass --plugin-json".format(tag))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("tag", help="the release tag, such as v0.1.0-rc.1")
    where = parser.add_mutually_exclusive_group(required=True)
    where.add_argument("--base-url", help="the URL DIR will be served from")
    where.add_argument("--github", metavar="OWNER/REPO[:BRANCH]",
                       help="DIR will be committed at the root of BRANCH (default main) of this public GitHub "
                            "repository: the base URL is https://raw.githubusercontent.com/OWNER/REPO/BRANCH")
    parser.add_argument("-o", "--output", default="test-extension-repo", metavar="DIR",
                        help="directory to write (default: %(default)s)")
    parser.add_argument("--repo", default=DEFAULT_REPOSITORY, help="GitHub owner/name (default: %(default)s)")
    parser.add_argument("--plugin-json", help="read this plugin.json instead of the tag's")
    parser.add_argument("--package-url", help="the plugin's source archive (default: the tag's zip on GitHub)")
    parser.add_argument("--created", type=int, help="Unix time of the release (default: now)")
    args = parser.parse_args()

    if not args.tag.startswith("v") or core.VERSION.match(args.tag[1:]) is None:
        fail("tag {!r} is not v<version>, where the version is {}".format(args.tag, core.VERSION_FORMAT))
    if core.REPOSITORY.match(args.repo) is None:
        fail("--repo {!r} is not owner/name".format(args.repo))
    if args.github is not None:
        try:
            args.base_url = github_base_url(args.github)
        except ValueError as e:
            fail(str(e))
    base = urllib.parse.urlsplit(args.base_url)
    if base.scheme not in ("http", "https") or not base.netloc or base.query or base.fragment:
        fail("--base-url {!r} is not an http(s) URL".format(args.base_url))
    base_url = args.base_url if args.base_url.endswith("/") else args.base_url + "/"

    plugin = read_plugin_json(args.plugin_json, args.tag)
    url = args.package_url or package_url(args.repo, args.tag)
    created = int(time.time()) if args.created is None else args.created
    try:
        documents = repository(plugin, args.repo, args.tag, base_url, url, created)
    except (ValueError, TypeError, KeyError) as e:
        fail(str(e))
    for name, document in documents.items():
        path = os.path.join(args.output, *name.split("/"))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(document, f, indent=2)
            f.write("\n")
    print("Wrote a repository of {} {} (from {}) to {}.".format(plugin["name"], plugin["version"], url, args.output))
    if args.github is not None:
        print("Commit the contents of {} at the root of {} and add this URL to the "
              "extensionManager.unofficialUrls setting:".format(args.output, args.github))
    else:
        print("Serve {} at {} and add this URL to the extensionManager.unofficialUrls setting:".format(
            args.output, base_url))
    print("  " + base_url.rstrip("/"))


if __name__ == "__main__":
    main()
