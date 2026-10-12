# Development

Building, testing and regenerating the data bundled in binja-espressif.
See [reference.md](reference.md) for how the plugin behaves and
[releasing.md](releasing.md) for cutting a release.

- [Repository layout](#repository-layout)
- [Building](#building)
- [Testing](#testing)
- [CI](#ci)
- [Regenerating bundled data](#regenerating-bundled-data)

## Repository layout

```
Cargo.toml          the workspace, and the plugin the release ships (binja-espressif)
src/lib.rs          its entry points: registers the three plugins below
arch/xtensa/        arch_xtensa: the Xtensa architectures, lifting, calling
                    conventions, loops, patching, ELF relocations
arch/riscv_ilp32/   arch_riscv_ilp32: the ilp32 calling convention for rv32gc
view/espressif/     view_espressif: the ESP view and flash dump container, ELF
                    fix-ups and chip recognition, chip platforms, ROM symbols
                    (data/rom_symbols), types (data/rom_types) and images, SVDs (svd/)
xtensa/             the xtensa decoder crate
binja_espressif_loader/, __init__.py, plugin.json
                    the extension manager's Python loader
scripts/            generators, fetchers and release tooling
tests/              end-to-end tests (headless Binary Ninja) and their data
docs/               this file, the technical reference and the release process
```

Each crate in `arch/` and `view/` follows the layout of binaryninja-api's
Rust plugins (`arch/msp430`, `arch/riscv`, `view/minidump`): a `Cargo.toml`
with `crate-type = ["cdylib", "rlib"]`, a `build.rs` that links the core, a
`CMakeLists.txt` that builds it with `bn_add_rust_crate` in the
binaryninja-api tree, and a README
([arch_xtensa](../arch/xtensa/README.md),
[arch_riscv_ilp32](../arch/riscv_ilp32/README.md),
[view_espressif](../view/espressif/README.md)). Built on its own, each is a
Binary Ninja plugin with its own `CorePluginInit` and
`CorePluginDependencies`.

The root package links them as Rust libraries with their `bundled`
feature, which leaves those entry points out, and registers them in the
order their plugin dependencies would load them: `arch_xtensa`,
`arch_riscv_ilp32`, `view_espressif`. So the release still ships one
library per platform for the extension manager's loader.

They share no Rust types: `view_espressif` looks the architectures and the
`ilp32` convention up by name, and the architectures know nothing of
Espressif's chips. (They do share core state: `arch_xtensa` leaves alone
the functions whose type the view imported from a ROM type library.)
Dependencies they have in common (`binaryninja`, `binaryninjacore-sys`, the
decoder) are `[workspace.dependencies]` of the root `Cargo.toml`.

A workspace build enables `bundled` on every crate (Cargo unifies
features), so the crates' own entry points are only compiled in a build of
the crate alone (`-p arch_xtensa`), as CI does.

## Building

Requirements:

- a Rust toolchain at least as new as the pinned binaryninja-api revision's
  `rust-version` (1.98.1 at the time of writing);
- libclang for `bindgen` (`libclang-dev` on Debian/Ubuntu);
- either a Binary Ninja installation whose core matches the API revision
  pinned in `Cargo.toml` (`binaryninjacore-sys` finds it through
  `~/.binaryninja/lastrun` or `~/binaryninja`; set `BINARYNINJADIR` to
  override), or, without the default `core` feature
  (`--no-default-features`), CMake 3.28+ and Ninja, which
  `binaryninjacore-sys` uses to build a stub core library to link instead.

```sh
cargo build --release
cp target/release/libbinja_espressif.so ~/.binaryninja/plugins/   # .dylib / .dll elsewhere
```

This builds the plugin the release ships, which holds all three crates;
install only this library. Each crate also builds as a plugin of its own
(`cargo build --release -p arch_xtensa -p arch_riscv_ilp32 -p
view_espressif`), as Binary Ninja's own build would make them: those builds
are for building the crates inside binaryninja-api, not for installing next
to `libbinja_espressif.so`.

A self-built library in the plugin folder takes precedence: if the
extension manager's loader is installed too, it sees the library and
installs nothing. Don't copy a self-built library into the plugin folder
next to an installed `binja_espressif-native.so`; remove that first.

On Windows, run `git config --global core.longpaths true` first:
binaryninja-api's `vendor/bindiff` submodule has paths longer than 260
characters, which cargo refuses to check out otherwise. The release build
sets it, and a short `CARGO_HOME`, for the same reason.

`view/espressif/build.rs` compresses the SVD files in `view/espressif/svd/`
into the plugin (see [SVD files](#svd-files)).

### Building for stable Binary Ninja

A native plugin loads only in a Binary Ninja whose core ABI matches the API
it was built against (`BN_MINIMUM_CORE_ABI_VERSION` to
`BN_CURRENT_CORE_ABI_VERSION` in `binaryninjacore.h`): a build for a 6.0
stable core (ABI 187) is refused by a 6.1 dev core (ABI 188-189), and the
other way round. `Cargo.toml` on `main` pins a `dev` revision of the API,
so a plain `cargo build` (like a release build) is for a dev Binary Ninja.

`scripts/use_stable_api.sh` rewrites both pins in the root `Cargo.toml`'s
`[workspace.dependencies]` to the stable tag it names and updates
`Cargo.lock`; run it (and don't commit the result) to build for a stable
Binary Ninja:

```sh
scripts/use_stable_api.sh
cargo build --release
```

To move to a newer dev revision, update the `rev` of `binaryninja` and
`binaryninjacore-sys` in the root `Cargo.toml`'s
`[workspace.dependencies]` (and `plugin.json`'s
`minimumbinaryninjaversion`, see [releasing.md](releasing.md)); to move to
a newer stable release, update `STABLE_TAG` in `scripts/use_stable_api.sh`.

## Testing

```sh
# Unit tests: image formats, partitions, loops, intrinsics, patching, relocations
cargo test -p arch_xtensa -p arch_riscv_ilp32 -p view_espressif -p binja-espressif --lib --no-default-features
# Lifts ~1.5M encodings through the real core
cargo test --release -p arch_xtensa --features liftcheck --lib liftcheck::lift_sweep
cargo build --release && tests/test_headless.py
tests/test_loader.py                      # the Python loader and release tooling
tests/test_fetch_rom_elfs.py              # scripts/fetch_rom_elfs.py
tests/test_gen_rom.py                     # the ROM generators' conflicting-name rule
cargo build --release && tests/test_loader_headless.py
cargo test -p xtensa                      # the decoder (see xtensa/README.md)
```

### Unit tests and the lift sweep

The unit tests link a stub core generated by `binaryninjacore-sys` (which
needs CMake 3.28+ and Ninja), so they need no Binary Ninja installation.
The lift sweep and the `test_headless` scripts need a headless-capable
Binary Ninja license.

The sweep is a unit test behind the `liftcheck` feature of `arch_xtensa`
(see `arch/xtensa/src/liftcheck.rs`), which also lists the decoded operands
of every opcode, for checking what the lifter assumes about them:

```sh
cargo test -p arch_xtensa --no-default-features --features liftcheck --lib liftcheck::operands -- --ignored --nocapture
```

`view_espressif`'s ignored `real_rom_elfs` test checks Espressif's real ROM
ELF files in `$BN_ESPRESSIF_ROM_ELFS` (an unpacked esp-rom-elfs release):

```sh
BN_ESPRESSIF_ROM_ELFS=/path/to/esp-rom-elfs cargo test -p view_espressif --no-default-features --lib real_rom_elfs -- --ignored
```

### End-to-end tests

`tests/test_headless.py` runs Binary Ninja with a throwaway user directory
containing only this plugin and checks ELF loading, chip detection, calling
conventions, call-site arguments, loop recovery, lifting semantics,
patching, and the ESP view (including partition selection and malformed
images) and the flash dump container on images and flash dumps synthesized
from `tests/data/*.elf`.

- Those ELFs are built from `tests/data/corpus.c` by
  `scripts/build_test_elfs.sh` with Espressif's GCC, which also builds the
  relocatable objects in `tests/data/obj` (with and without `-mlongcalls`,
  ESP-IDF style with `-ffunction-sections`, and `tests/data/reloc_edge.S`,
  whose relocations cannot all be applied) and lists their relocations with
  objdump: the test checks every `R_XTENSA_SLOT0_OP` and `R_XTENSA_32`
  Binary Ninja applies against that listing.
- `tests/data/idf_*.elf`, built from `tests/data/idf_features.c` by
  `scripts/build_idf_elfs.sh`, are laid out like ESP-IDF applications
  (`idf_esp32c6.elf` with the C6's `.flash_rodata_dummy` over its code,
  `idf_esp32s3.elf` with the S3's `.ext_ram.dummy` over its rodata) and
  call into the mask ROM: the test checks the ELF fix-ups and the ROM types
  (the arguments of ROM calls and libgcc helpers, a ROM structure's fields,
  that reopening a database changes nothing, and that a database typed from
  another version of the library is brought up to date) on them and on
  images made from them.
- The build scripts use Espressif's GCC from `xtensa/target/oracle/` by
  default (or take the toolchain directories as arguments). The Xtensa
  toolchain is the one the decoder's objdump oracle uses:
  `xtensa/scripts/oracle/fetch-toolchain.sh xtensa/target/oracle` installs
  it there. The RISC-V one comes
  from `scripts/fetch_riscv_toolchain.sh`, which installs there by
  default.
- The user directory (`bn-espressif-test-*` in the system temp directory)
  also holds the databases and images the test writes, and is removed when
  the test exits; set `BN_ESPRESSIF_KEEP_TMP=1` to keep it for debugging
  (the test prints its path).

Those checks run without ROM images (ESP-IDF's are hidden from the
plugin). The mask ROM image checks need the pinned esp-rom-elfs archive,
which the test does not download:

```sh
curl -LO https://github.com/espressif/esp-rom-elfs/releases/download/20260528/esp-rom-elfs-20260528.tar.gz
BN_ESPRESSIF_ROM_TARBALL=$PWD/esp-rom-elfs-20260528.tar.gz tests/test_headless.py
```

They check the ROM types again with a directory of ROM ELF files (and in
databases), that the ROM's functions are created and analyzed (or only
named, with `espressif.rom.createFunctions` off), the revision chosen,
`espressif.rom.elfDirectory` as a load option, the `espressif.rom`
metadata, a view that maps an image found after it was loaded, and the
download: served from a local HTTP server (through the
`BN_ESPRESSIF_ROM_TEST_URL` override, loopback only, with
`BN_ESPRESSIF_ROM_TEST_TIMEOUT` shortening the wait), not without consent,
not when corrupted, and through the command.

### Loader tests

`tests/test_loader.py` needs only Python (it stands in for the
`binaryninja` module where it needs one) and runs in CI, as do
`tests/test_fetch_rom_elfs.py`, which checks `scripts/fetch_rom_elfs.py` on
small synthetic archives, and `tests/test_gen_rom.py`.

`tests/test_loader_headless.py` installs the loader into a throwaway user
directory with a manifest listing the locally built library as the release
asset for the running core's ABI, serves it from a local HTTP server (the
loader fetches from `$BN_ESPRESSIF_LOADER_TEST_BASE_URL` instead of GitHub;
it accepts only loopback addresses there, and still checks the manifest's
hash), and runs headless Binary Ninja several times. It checks that nothing
is downloaded without `espressif.loader.autoDownload`, that with it the
library lands in the user plugin folder as `binja_espressif-native.so` and
loads after a restart, and that a tampered asset or a self-built copy in
the plugin folder means nothing is installed.

## CI

`.github/workflows/ci.yml` runs on every push to `main` and every pull
request, on `ubuntu-latest`, without Binary Ninja (stub core):

- **build-and-test**: builds the workspace, runs the unit tests, and builds
  each plugin crate on its own, checking it exports `CorePluginInit`.
- **decoder**: the `xtensa` crate's tests and docs on stable, beta and its
  minimum supported Rust version (`rust-version` in `xtensa/Cargo.toml`),
  skipping the exhaustive tests.
- **lint**: `cargo fmt --check` and clippy (`-D warnings`) on the
  workspace, the `liftcheck` feature and each crate alone.
- **loader**: `tests/test_loader.py`, `tests/test_fetch_rom_elfs.py` and
  `tests/test_gen_rom.py`.
- **stable-api**: builds, lints and tests against the stable API
  (`scripts/use_stable_api.sh`).

The lift sweep and the end-to-end tests need a Binary Ninja license, so
they don't run in CI; run them locally before changing the lifter, the view
or the loader.

`.github/workflows/nightly.yml` runs the decoder's slow checks daily: the
exhaustive decode of every encoding against objdump-verified digests
(`xtensa/tests/exhaustive.rs`), and a comparison of every encoding against
objdump itself (`xtensa/scripts/oracle/run.sh`), which also checks the
committed fixtures are up to date.

`.github/workflows/release.yml` builds and publishes releases; see
[releasing.md](releasing.md).

## Regenerating bundled data

### ROM symbol tables

The ROM symbol tables in `view/espressif/data/rom_symbols/` (one text file
per chip, embedded in the plugin; `view/espressif/src/rom_symbols.rs`
describes the format) are generated from the ROM linker scripts of ESP-IDF
v6.1 by `scripts/gen_rom_symbols.py` (see its header for the sources and
the exact commands). The Xtensa chips' keep the names ESP-IDF v5.3 gave
and v6.1 dropped or renamed (`gpio_matrix_out`, now
`rom_gpio_matrix_out`). It reads Espressif's ROM ELF files too
(`--rom-elfs`, fetched by `scripts/fetch_rom_elfs.py`): where two linker
scripts name one address differently, the names the ROM does not give it
are left out (ESP-IDF's `esp32s3.rom.bt_funcs.ld` puts Bluetooth hooks at
the ROM's CRC and MD5 functions), and the type libraries do not type them.

### ROM type libraries

The ROM type libraries in `view/espressif/data/rom_types/` are generated,
offline, by `scripts/gen_rom_types.py`, which runs headless Binary Ninja
with a plugin build (for the chip platforms) and parses the headers of the
ESP-IDF version named in it, copied out of the `espressif/idf` docker image
by `scripts/fetch_rom_type_headers.sh`. The names that earlier releases'
linker scripts give the same ROM functions, which firmware built with them
carries, are typed too (`--older`):

```sh
cargo build --release
scripts/fetch_rom_type_headers.sh /tmp/rom-headers
scripts/fetch_rom_type_headers.sh /tmp/rom-headers-v5.5.5 v5.5.5
scripts/fetch_rom_type_headers.sh /tmp/rom-headers-v5.4.1 v5.4.1
scripts/gen_rom_types.py --older /tmp/rom-headers-v5.5.5 --older /tmp/rom-headers-v5.4.1 \
    /tmp/rom-headers esp32 esp32s2 esp32s3 esp32c2 esp32c3 esp32c6 esp32h2 esp32c5
cargo build --release   # embeds the new libraries
```

Each library records the ESP-IDF version and the list of headers it was
made from in its metadata (`espressif.idf_version`, `espressif.headers`):
the chip's `components/esp_rom/<chip>/include/<chip>/rom/*.h`, esp_rom's
chip-neutral `components/esp_rom/include/*.h`, the heap component's
`rom_patch_tlsf.h` and `rom_patch_multi_heap.h`, `xtensa/xtruntime.h` and
`xtensa/hal.h` (Xtensa) or `riscv/interrupt.h` (RISC-V), newlib's
`string.h`, `strings.h`, `stdio.h`, `stdlib.h`, `ctype.h`, `time.h`,
`sys/unistd.h`, `sys/_default_fcntl.h`, `sys/lock.h`, `reent.h`,
`sys/reent.h`, `setjmp.h`, `locale.h`, `sys/time.h`, `sys/stat.h`,
`signal.h`, `assert.h`, `envlock.h` and `malloc.h`, and
`scripts/rom_types_extra.h`.

### ROM images

The esp-rom-elfs release the plugin downloads is pinned, with its size and
SHA-256, in `view/espressif/src/rom_images.rs`; `scripts/fetch_rom_elfs.py`
reads it from there. Moving to a newer release means updating that pin, the
archive URL in the [end-to-end test instructions](#end-to-end-tests), and
regenerating the ROM symbol tables (all but the ESP8266's are made with
it).

### SVD files

The SVD files in `view/espressif/svd/` are vendored, each pinned to an
upstream commit with its SHA-256 in `view/espressif/svd/SOURCES`;
`scripts/fetch_svds.sh` downloads them again and checks them (to pick up a
newer file, change its commit there and run the script with `--update`).
The ESP8266's is generated with svdtools 0.5.0 from esp-pacs' file and
patches and `view/espressif/svd/esp8266.yaml`, which the script does again.
See [view/espressif/svd/README.md](../view/espressif/svd/README.md) for the
sources, the ESP8266 patches and how to update them.

### Exemplars

`arch/xtensa/src/exemplars.rs` holds one sample encoding of every mnemonic
of each core, from which the architectures derive each intrinsic's
declared inputs and outputs. It is generated from the decoder's
objdump-verified fixtures (`xtensa/tests/data/<chip>.tsv`):

```sh
scripts/gen_exemplars.py > arch/xtensa/src/exemplars.rs
```

Regenerate it after the decoder gains instructions.
