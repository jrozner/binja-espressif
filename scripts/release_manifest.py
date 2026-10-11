#!/usr/bin/env python3
"""Release tooling for the loader's manifest (binja_xtensa_loader/manifest.json).

Used by .github/workflows/release.yml; see "Releasing" in the README.

  generate DIST --version V --repository R --commit C --run-id N --api dev|stable [--api-source S]
      Prints the manifest for the libraries in DIST (the build run's
      artifacts), recording the binaryninja-api they were built against
      (and, optionally, its Cargo source, revision included).
  check-tag TAG [--prerelease]
      Checks that TAG is v<version>, with a pre-release version
      (0.2.0-rc.1) if and only if --prerelease is given; that plugin.json
      and the committed manifest are both for TAG; that the manifest lists
      assets; and prints the build's commit and run ID as KEY=value lines
      (for $GITHUB_OUTPUT).
  check-tree BUILD_COMMIT TAG
      Checks that the build commit is an ancestor of the tag and that the
      tag's tree equals the build commit's apart from the manifest and
      plugin.json's version, so the published libraries were built from
      the source the tag ships.
  verify DIST
      Checks that DIST holds exactly the committed manifest's assets with
      their sizes and SHA-256 hashes, and writes DIST/SHA256SUMS.
"""

import argparse
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from binja_xtensa_loader import core  # noqa: E402

MANIFEST = "binja_xtensa_loader/manifest.json"
PLUGIN_JSON = "plugin.json"
# What may differ between the build commit and the tag.
RELEASE_FILES = {MANIFEST, PLUGIN_JSON}


def fail(message):
    print("error: " + message, file=sys.stderr)
    sys.exit(1)


def generate(dist, version, repository, commit, run_id, api, api_source=None):
    if core.VERSION.match(version) is None:
        fail("version {!r} is not {}".format(version, core.VERSION_FORMAT))
    assets = []
    for name in sorted(os.listdir(dist)):
        path = os.path.join(dist, name)
        if not os.path.isfile(path):
            continue
        assets.append(core.parse_asset({
            "name": name, "sha256": core.sha256_file(path), "size": os.path.getsize(path)}))
    if not assets:
        fail("no libraries in " + dist)
    build = {"commit": commit, "run_id": int(run_id), "api": api}
    if api_source:
        build["api_source"] = api_source
    manifest = core.Manifest(version=version, repository=repository, assets=tuple(assets),
                             build=build)
    text = core.manifest_json(manifest)
    core.parse_manifest(text)  # the loader must accept what's generated
    sys.stdout.write(text)


def check_tag_name(tag, prerelease):
    """Checks that TAG is a release tag, and a pre-release one exactly when
    the release is to be published as a pre-release."""
    if not tag.startswith("v") or core.VERSION.match(tag[1:]) is None:
        fail("tag {!r} is not v<version>, where the version is {}".format(tag, core.VERSION_FORMAT))
    if core.is_prerelease(tag[1:]) and not prerelease:
        fail("tag {} is a pre-release version: publish it as a pre-release (the prerelease input), "
             "or tag a final version".format(tag))
    if prerelease and not core.is_prerelease(tag[1:]):
        fail("tag {} is not a pre-release version (such as {}-rc.1), so it cannot be published as a "
             "pre-release".format(tag, tag))


def check_tag(tag, prerelease, root=ROOT):
    check_tag_name(tag, prerelease)
    manifest = core.load_manifest(os.path.join(root, MANIFEST))
    version = core.read_plugin_version(os.path.join(root, PLUGIN_JSON))
    if "v" + version != tag:
        fail("plugin.json version {} does not match tag {}".format(version, tag))
    if manifest.tag != tag:
        fail("{} is for version {}, not tag {}".format(MANIFEST, manifest.version, tag))
    if not manifest.assets or manifest.build is None:
        fail("{} lists no assets: commit the manifest of a build run first".format(MANIFEST))
    print("commit={}".format(manifest.build["commit"]))
    print("run_id={}".format(manifest.build["run_id"]))


def git(root, *args):
    return subprocess.run(("git",) + args, cwd=root, check=True, stdout=subprocess.PIPE).stdout


def check_tree(build_commit, tag, root=ROOT):
    if subprocess.run(["git", "merge-base", "--is-ancestor", build_commit, tag + "^{commit}"], cwd=root).returncode:
        fail("build commit {} is not an ancestor of tag {}".format(build_commit, tag))
    changed = set(git(root, "diff", "--name-only", "--no-renames", build_commit, tag + "^{commit}").decode().split("\n")) - {""}
    unexpected = sorted(changed - RELEASE_FILES)
    if unexpected:
        fail("the tag's tree differs from the build commit's in more than the release files: "
             + ", ".join(unexpected))
    before = json.loads(git(root, "show", "{}:{}".format(build_commit, PLUGIN_JSON)))
    after = json.loads(git(root, "show", "{}:{}".format(tag + "^{commit}", PLUGIN_JSON)))
    before.pop("version", None)
    after.pop("version", None)
    if before != after:
        fail("plugin.json differs between the build commit and the tag in more than its version")
    print("tag {} matches build commit {} apart from {}".format(tag, build_commit, ", ".join(sorted(changed)) or "nothing"))


def verify(dist, manifest_path):
    manifest = core.load_manifest(manifest_path)
    expected = {a.name: a for a in manifest.assets}
    present = {n for n in os.listdir(dist) if os.path.isfile(os.path.join(dist, n))} - {"SHA256SUMS"}
    if present != set(expected):
        fail("artifacts {} do not match the manifest's assets {}".format(sorted(present), sorted(expected)))
    for name, asset in sorted(expected.items()):
        path = os.path.join(dist, name)
        size, digest = os.path.getsize(path), core.sha256_file(path)
        if size != asset.size or digest != asset.sha256:
            fail("{}: size {} sha256 {}, the manifest lists size {} sha256 {}".format(
                name, size, digest, asset.size, asset.sha256))
        print("ok {} {}".format(digest, name))
    with open(os.path.join(dist, "SHA256SUMS"), "w") as f:
        for name, asset in sorted(expected.items()):
            f.write("{}  {}\n".format(asset.sha256, name))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("generate")
    p.add_argument("dist")
    p.add_argument("--version", required=True)
    p.add_argument("--repository", required=True)
    p.add_argument("--commit", required=True)
    p.add_argument("--run-id", required=True, type=int)
    p.add_argument("--api", required=True, choices=core.API_CHANNELS,
                   help="the binaryninja-api built against: Cargo.toml's dev pin, or the stable release")
    p.add_argument("--api-source", help="the Cargo source of binaryninjacore-sys built against")
    p = sub.add_parser("check-tag")
    p.add_argument("tag")
    p.add_argument("--prerelease", action="store_true", help="the release is published as a pre-release")
    p.add_argument("--root", default=ROOT, help=argparse.SUPPRESS)
    p = sub.add_parser("check-tree")
    p.add_argument("build_commit")
    p.add_argument("tag")
    p.add_argument("--root", default=ROOT, help=argparse.SUPPRESS)
    p = sub.add_parser("verify")
    p.add_argument("dist")
    p.add_argument("--manifest", default=os.path.join(ROOT, MANIFEST), help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if args.command == "generate":
            generate(args.dist, args.version, args.repository, args.commit, args.run_id, args.api,
                     args.api_source)
        elif args.command == "check-tag":
            check_tag(args.tag, args.prerelease, args.root)
        elif args.command == "check-tree":
            check_tree(args.build_commit, args.tag, args.root)
        else:
            verify(args.dist, args.manifest)
    except core.LoaderError as e:
        fail(str(e))
    except subprocess.CalledProcessError as e:
        fail(str(e))


if __name__ == "__main__":
    main()
