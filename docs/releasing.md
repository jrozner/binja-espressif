# Releasing

How a release of binja-espressif is built and published, and how to test
one through the extension manager before the plugin is listed on
extensions.binary.ninja. See [development.md](development.md) for building
and testing from a checkout.

- [How a release works](#how-a-release-works)
- [Cutting a release](#cutting-a-release)
- [Pre-releases](#pre-releases)
- [Testing a release privately](#testing-a-release-privately)

## How a release works

The extension manager installs only the plugin's Python loader, from the
source of the release tag; the loader then downloads the native library for
the running platform from the GitHub release and installs it only if its
size and SHA-256 match `binja_espressif_loader/manifest.json` in that
source (see [reference.md](reference.md#the-native-library-loader)).

A tag cannot contain the hashes of libraries built from itself, so a
release is built from one commit and published from a tag on a commit that
adds only the manifest, by `.github/workflows/release.yml` (manual only) in
two runs. `scripts/release_manifest.py` does the generating and checking.

Release assets are one library per platform, named after the core ABI they
were built for:

```
binja_espressif-abi<N>-linux-x86_64.so
binja_espressif-abi<N>-windows-x86_64.dll
binja_espressif-abi<N>-macos-universal.dylib
```

The loader picks the one the running core accepts, so the publish run
doesn't need to know the API. The builds link a stub core (the `core`
feature is off), so no Binary Ninja installation or license is needed in
CI.

Between releases, `main`'s manifest stays that of the last release; if
`plugin.json`'s version moves ahead of it, the loader of a checkout of
`main` refuses to install anything, as it isn't a release.

## Cutting a release

1. **Build.** Run the Release workflow on `main` (or another branch) with
   mode `build`, and optionally the version being released (default:
   `plugin.json`'s) and the API to build against:

   ```sh
   gh workflow run release.yml --ref main -f mode=build -f version=0.2.0
   ```

   The `api` input is `dev` by default: the binaryninja-api revision pinned
   in `Cargo.toml`, for the Binary Ninja dev channel. `-f api=stable` builds
   against the stable release named in `scripts/use_stable_api.sh` instead;
   releases will move to (or add) stable later. Either way, set
   `plugin.json`'s `minimumbinaryninjaversion` in the build commit to the
   oldest Binary Ninja build the libraries load in (the extension manager
   hides the plugin from older ones): for the dev API, the first dev build
   whose core accepts the pinned revision's core ABI (10874 for ABI 189,
   which those builds accept as 188-189); for stable, the stable build
   number (10601 for 6.0).

   It builds the dispatched commit for Linux, Windows and macOS, uploads
   the libraries as artifacts of the run (kept 90 days), and generates the
   loader's manifest: the version, the build commit, the run ID, the API
   built against (`build.api` and `build.api_source`, for the record), and
   each library's name, core ABI, size and SHA-256. The run summary shows
   it and each platform's core ABI, and the `manifest` artifact holds it.
   The platforms build independently, so one failing doesn't cancel the
   others, but the manifest is generated only when all three succeed.

2. **Commit and tag.** On top of the build commit, save the manifest as
   `binja_espressif_loader/manifest.json`, set `plugin.json`'s `version` to
   the release version if it isn't already, and change nothing else.
   Commit that on top of the build commit (through a PR works too, as long
   as nothing else lands on `main` in between), then tag the commit
   `v<version>` and push the tag.

3. **Publish.** Run the workflow on the tag with mode `publish`:

   ```sh
   gh workflow run release.yml --ref v0.2.0 -f mode=publish
   ```

   It checks that `plugin.json` and the committed manifest are for the
   tag; that the manifest's build commit is an ancestor of the tag whose
   tree differs only in the manifest and `plugin.json`'s version; and that
   the manifest's run is a successful dispatch of this workflow on that
   commit. It then downloads that run's artifacts, checks them against the
   committed manifest (exactly the listed files, with their sizes and
   hashes), and creates the GitHub release from them, with a `SHA256SUMS`.

## Pre-releases

A version with a pre-release suffix, such as `0.2.0-rc.1` (tag
`v0.2.0-rc.1`), goes through the same steps, and its publish run needs the
`prerelease` input (`-f prerelease=true`); a final version's must not have
it. The release is then a GitHub pre-release and is not marked latest.

That matters beyond testing: extensions.binary.ninja reads `plugin.json` at
the latest release, skips a version it already has, and ignores a
pre-release suffix when comparing versions, so an rc it picked up could
make it skip the final release of the same version.

Commit a pre-release's manifest on a branch rather than `main`, so `main`
keeps the final version:

```sh
gh workflow run release.yml --ref main -f mode=build -f version=0.2.0-rc.1
# on top of the build commit, on a branch: commit the run's manifest as
# binja_espressif_loader/manifest.json with plugin.json's version set to
# 0.2.0-rc.1, tag that commit v0.2.0-rc.1 and push the tag
gh workflow run release.yml --ref v0.2.0-rc.1 -f mode=publish -f prerelease=true
```

To release exactly the libraries a pre-release was tested with, reuse its
build run: on top of the same build commit, commit its manifest with only
`version` changed (say to `0.2.0`) and `plugin.json`'s version to match,
tag that `v0.2.0` and publish it without `prerelease` (within the 90 days
the run's artifacts are kept).

## Testing a release privately

Before the plugin is listed on extensions.binary.ninja, a release can be
installed through the extension manager from a repository of your own.

Release libraries are built for the core ABI of the Binary Ninja dev
channel, so test with a **dev** Binary Ninja whose core accepts the
release's ABI (see `minimumbinaryninjaversion` above). A newer dev build may
have moved on: if its minimum core ABI is above the release's, the loader
logs that no prebuilt library matches (listing the platforms and ABIs the
release has) and installs nothing. Then update the `rev` pins in
`Cargo.toml` to the binaryninja-api revision of that build
(`api_REVISION.txt` in the Binary Ninja install), set
`minimumbinaryninjaversion` to match, and cut a new rc. A stable Binary
Ninja finds no library to install.

1. **Cut a pre-release** (see [Pre-releases](#pre-releases)).

2. **Make a repository** listing that release. Binary Ninja reads
   third-party repositories in the format of the extensions.binary.ninja API
   (as served by Binary Ninja's `examples/python/mock_extension_server.py`),
   not the `plugins.json` of Vector35/community-plugins, which current
   versions reject. `scripts/test_extension_repo.py` writes it as static
   files from `plugin.json` at the tag, with the tag's source archive on
   GitHub as the download, the way the extension server would list it.

   The simplest host is a **public** GitHub repository (or a branch of
   one), such as
   [jrozner/binja-extensions-test](https://github.com/jrozner/binja-extensions-test):
   with `--github OWNER/REPO[:BRANCH]` (branch `main` by default), the
   files are written for the root of that branch, served by
   raw.githubusercontent.com:

   ```sh
   git fetch --tags
   git clone https://github.com/jrozner/binja-extensions-test ../binja-extensions-test
   scripts/test_extension_repo.py v0.2.0-rc.1 --github jrozner/binja-extensions-test -o ../binja-extensions-test
   git -C ../binja-extensions-test add v2/manifests extensions.json versions.json
   git -C ../binja-extensions-test commit -m "List binja-espressif 0.2.0-rc.1"
   git -C ../binja-extensions-test push
   ```

   The repository URL is then
   `https://raw.githubusercontent.com/jrozner/binja-extensions-test/main`. A
   private repository does not work, as Binary Ninja fetches without
   credentials. raw.githubusercontent.com caches files for about 5 minutes,
   so after pushing an update (such as the final release, below), wait that
   long before Binary Ninja checks for updates.

   To keep it on the test machine instead, generate it with the URL it
   will be served from and serve the directory locally:

   ```sh
   scripts/test_extension_repo.py v0.2.0-rc.1 --base-url http://127.0.0.1:8000 -o bx-repo
   python3 -m http.server 8000 --bind 127.0.0.1 --directory bx-repo
   ```

   Any host that serves the files at their paths works with `--base-url`
   (not a gist: Binary Ninja fetches `<base URL>/v2/manifests`).

3. **Start a dev Binary Ninja with a throwaway user folder**, holding only
   your license and the repository URL. Binary Ninja uses the folder named
   by `BN_USER_DIRECTORY` in place of its
   [user folder](https://docs.binary.ninja/guide/index.html#user-folder),
   whose `license.dat` you copy. Quit Binary Ninja first, so the new
   instance starts with this environment.

   macOS (Terminal):

   ```sh
   mkdir bn-test && cp ~/Library/Application\ Support/Binary\ Ninja/license.dat bn-test/
   echo '{"extensionManager.unofficialUrls": ["https://raw.githubusercontent.com/jrozner/binja-extensions-test/main"]}' > bn-test/settings.json
   BN_USER_DIRECTORY=$PWD/bn-test /Applications/Binary\ Ninja.app/Contents/MacOS/binaryninja
   ```

   Windows (PowerShell):

   ```powershell
   mkdir bn-test; copy "$env:APPDATA\Binary Ninja\license.dat" bn-test\
   Set-Content bn-test\settings.json '{"extensionManager.unofficialUrls": ["https://raw.githubusercontent.com/jrozner/binja-extensions-test/main"]}'
   $env:BN_USER_DIRECTORY = "$PWD\bn-test"
   & "C:\Program Files\Vector35\BinaryNinja\binaryninja.exe"
   ```

   Windows (cmd):

   ```bat
   mkdir bn-test && copy "%APPDATA%\Binary Ninja\license.dat" bn-test\
   echo {"extensionManager.unofficialUrls": ["https://raw.githubusercontent.com/jrozner/binja-extensions-test/main"]} > bn-test\settings.json
   set "BN_USER_DIRECTORY=%CD%\bn-test"
   "C:\Program Files\Vector35\BinaryNinja\binaryninja.exe"
   ```

   A per-user Windows install is in `%LOCALAPPDATA%\Vector35\BinaryNinja`
   instead. On Linux, the license is in `~/.binaryninja` and the binary is
   wherever you extracted Binary Ninja (`BN_USER_DIRECTORY=$PWD/bn-test
   ~/binaryninja/binaryninja`). The variable applies only to what is
   started from that shell. With a local server, the URL is
   `http://127.0.0.1:8000`. The setting is *Unofficial 3rd Party Plugin
   Repository URLs* in Settings, too.

4. **Install** *Espressif* (the pre-release version, from the
   `binja-extensions-test` repository) in the extension manager and
   restart.

Then check:

- The loader asks to download the library for this platform; accept it.
  The log names the library (`binja_espressif-abi<N>-<platform>`) and its
  verified SHA-256, and `binja_espressif-native.so` (`.dll`, `.dylib`) is
  in `bn-test/plugins`. If it says no prebuilt library matches instead,
  your dev build's core ABI has moved on (see above).
- After a restart, the library loads without errors in the log and an
  ESP32 ELF or image opens with the Xtensa architecture.
- The ROM image prompt appears for a file that uses it, and
  **Plugins > Espressif > Download ROM Images** maps it.
- A flash dump opens in the container browser with its partitions, and
  headless, `files.espressif.partition` picks one.
- **Plugins > Espressif > Remove Native Library** removes the library
  (after a restart, the architectures are gone and the loader doesn't ask
  again), and uninstalling the plugin in the extension manager removes the
  loader.

To try the update to the final release, regenerate the repository for the
final tag once it is published (and push it, then wait out the cache): the
extension manager offers it as an update, and the loader then asks to
replace the library if it changed (not if the final release reuses the
pre-release's build). Delete `bn-test` when done.
