#!/usr/bin/env python3
"""Release tooling for the loader's manifest (binja_xtensa_loader/manifest.json).

Used by .github/workflows/release.yml; see "Releasing" in the README.

  generate DIST --version V --repository R --commit C --run-id N
      Prints the manifest for the libraries in DIST (the build run's
      artifacts).
  check-tag TAG
      Checks that plugin.json and the committed manifest are both for TAG,
      that the manifest lists assets, and prints the build's commit and
      run ID as KEY=value lines (for $GITHUB_OUTPUT).
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


def generate(dist, version, repository, commit, run_id):
    if core.VERSION.match(version) is None:
        fail("version {!r} is not X.Y or X.Y.Z".format(version))
    assets = []
    for name in sorted(os.listdir(dist)):
        path = os.path.join(dist, name)
        if not os.path.isfile(path):
            continue
        assets.append(core.parse_asset({
            "name": name, "sha256": core.sha256_file(path), "size": os.path.getsize(path)}))
    if not assets:
        fail("no libraries in " + dist)
    manifest = core.Manifest(version=version, repository=repository, assets=tuple(assets),
                             build={"commit": commit, "run_id": int(run_id)})
    text = core.manifest_json(manifest)
    core.parse_manifest(text)  # the loader must accept what's generated
    sys.stdout.write(text)


def check_tag(tag):
    manifest = core.load_manifest(os.path.join(ROOT, MANIFEST))
    version = core.read_plugin_version(os.path.join(ROOT, PLUGIN_JSON))
    if "v" + version != tag:
        fail("plugin.json version {} does not match tag {}".format(version, tag))
    if manifest.tag != tag:
        fail("{} is for version {}, not tag {}".format(MANIFEST, manifest.version, tag))
    if not manifest.assets or manifest.build is None:
        fail("{} lists no assets: commit the manifest of a build run first".format(MANIFEST))
    print("commit={}".format(manifest.build["commit"]))
    print("run_id={}".format(manifest.build["run_id"]))


def git(*args):
    return subprocess.run(("git",) + args, cwd=ROOT, check=True, stdout=subprocess.PIPE).stdout


def check_tree(build_commit, tag):
    if subprocess.run(["git", "merge-base", "--is-ancestor", build_commit, tag + "^{commit}"], cwd=ROOT).returncode:
        fail("build commit {} is not an ancestor of tag {}".format(build_commit, tag))
    changed =set(git("diff", "--name-only", "--no-renames", build_commit, tag + "^{commit}").decode().split("\n")) - {""}
    unexpected = sorted(changed - RELEASE_FILES)
    if unexpected:
        fail("the tag's tree differs from the build commit's in more than the release files: "
             + ", ".join(unexpected))
    before = json.loads(git("show", "{}:{}".format(build_commit, PLUGIN_JSON)))
    after = json.loads(git("show", "{}:{}".format(tag + "^{commit}", PLUGIN_JSON)))
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
    p = sub.add_parser("check-tag")
    p.add_argument("tag")
    p = sub.add_parser("check-tree")
    p.add_argument("build_commit")
    p.add_argument("tag")
    p = sub.add_parser("verify")
    p.add_argument("dist")
    p.add_argument("--manifest", default=os.path.join(ROOT, MANIFEST), help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if args.command == "generate":
            generate(args.dist, args.version, args.repository, args.commit, args.run_id)
        elif args.command == "check-tag":
            check_tag(args.tag)
        elif args.command == "check-tree":
            check_tree(args.build_commit, args.tag)
        else:
            verify(args.dist, args.manifest)
    except core.LoaderError as e:
        fail(str(e))
    except subprocess.CalledProcessError as e:
        fail(str(e))


if __name__ == "__main__":
    main()
