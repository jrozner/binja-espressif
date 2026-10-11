# binja-xtensa

A Binary Ninja architecture plugin for the Xtensa cores in Espressif's
**ESP8266**, **ESP32**, **ESP32-S2** and **ESP32-S3**, written in Rust against
the [binaryninja-api](https://github.com/Vector35/binaryninja-api): `main`
builds against its `dev` branch, releases against the latest stable release
(6.0).
Decoding is done by the [`xtensa`](xtensa/) disassembler crate in `xtensa/`,
which is verified against GNU objdump.

It also loads firmware for Espressif's RISC-V chips, the **ESP32-C2**,
**ESP32-C3**, **ESP32-C6**, **ESP32-H2** and **ESP32-C5**, whose code is
analyzed with Binary Ninja's own `rv32gc` architecture.

It provides:

- **Architectures** `xtensa-esp8266`, `xtensa-esp32`, `xtensa-esp32s2` and
  `xtensa-esp32s3`, each decoding exactly the instructions its core
  implements (including MAC16, booleans, FPU, the ESP32 DFP assist,
  the S2 dedicated-GPIO TIE and the S3 PIE vector extension), and a
  standalone platform of the same name.
- **Platforms** `esp8266`, `esp32`, `esp32s2`, `esp32s3`, `esp32c2`,
  `esp32c3`, `esp32c6`, `esp32h2` and `esp32c5` (the ESP-IDF target names,
  under the OS `espressif`), one per chip, on the chip's architecture and
  with its calling conventions. ESP images and ELF files
  open with these; the chip, for example for the SVD file, is taken from
  the platform. The architectures' standalone platforms remain available
  (e.g. for `loader.platform`), but do not identify a chip.
- **Disassembly** in GNU objdump syntax, with branch/call targets and `l32r`
  literals as address tokens.
- **Lifting** of the whole ISA to LLIL: every decodable instruction lifts
  (checked by `tests/lift_sweep.rs`), with native semantics for the integer,
  load/store, branch, call, shift/SAR, zero-overhead loop, boolean,
  floating point and conditional-move instructions, and intrinsics for the
  rest.
- **Flags**: the boolean registers `b0`–`b15` (Xtensa has no condition codes).
- **Intrinsics** named after their mnemonics (`__memw`, `__rsil`,
  `__ee_vmulas_s16_accx_ld_ip`, ...) and for accesses to hardware special and
  user registers (`__rsr_ccount()`, `__wsr_intenable(x)`, `__xsr_vecbase(x)`).
- **Calling conventions**: `windowed` (the `call8` ABI used for all ESP32
  family code; default there) and `call0` (the ESP8266 ABI, and hand-written
  or `-mabi=call0` code on the others). Functions that do not begin with
  `entry` on the windowed cores get `call0` before their parameters and
  return value are inferred, unless you have given the function a type or
  calling convention yourself (for example a windowed stub with no `entry`,
  typed `__convention("windowed")`). There, `callx0` sites are typed as
  `call0` calls (with the callee's type, or variadic for an unknown target),
  since Binary Ninja otherwise calls through a register with the platform's
  default convention; a call site type you set is kept, and so is the type
  of a function pointer called through (give it `__convention("call0")`).
  Both place arguments as GCC does: in the six argument registers, a value
  aligned to 8 bytes (`double`, 64-bit integers) starting at an even one and
  structures included, and on the stack from the first argument that does
  not fit. On `rv32gc`,
  `ilp32`, the soft-float ABI ESP-IDF builds the RISC-V chips for (the
  architecture's own `default` passes floating point values in `fa0`–`fa7`),
  is the RISC-V chip platforms' default.
- **Zero-overhead loops** (`loop`/`loopnez`/`loopgtz`) recovered as real loops.
- **Patching** on the Xtensa architectures: converting to nops (`nop`,
  `nop.n`), never branch, invert branch (every conditional branch, narrow
  `beqz.n`/`bnez.n` included) and always branch (a 3-byte branch becomes a
  `j` to the same target; a narrow branch has no room for one). Skip and
  return value turns a call into a `movi` of the value (-2048 to 2047) to
  the register the call returns in: `a2` after `call0`, `a10` after `call8`.
  Loop instructions and `entry` are not offered as branches.
- An **`ESP` binary view** for raw firmware:
  - ESP32/ESP32-S2/ESP32-S3 and ESP32-C2/C3/C6/H2/C5 application and
    bootloader images from any ESP-IDF version (the chip is taken from the
    extended header's `chip_id`; images from before that field existed are
    recognised as ESP32);
  - ESP8266 v1 images (NONOS/RTOS SDK, including the RTOS SDK v3 layout with an
    extended header) and v2 images (`0xEA`, NONOS OTA `user1.bin`, Arduino);
  - flash dumps, starting at flash offset 0 or 0x1000 (as `esptool merge_bin`
    and most firmware distributions produce for the ESP32/ESP32-S2), through
    the `EspFlash` container transform (see **Flash dumps** below). ESP8266
    NONOS dumps (no partition table: a v1 image at 0 that maps no flash
    itself, with data written past it from 0x10000) are mapped as a whole
    through the 0x40200000 flash cache window so `irom0` code is present.
    An ESP8266 app on its own, however long, is not.

  The view maps every segment into the chip's memory map with appropriate
  permissions and section semantics, adds unbacked segments for `.bss`,
  RTC memory and the peripheral register blocks, names the mask ROM's
  functions and data (from the ESP-IDF and ESP8266 SDK linker scripts),
  types them (see **Mask ROM types** below), names
  the ESP8266's peripheral blocks, types the ESP-IDF `esp_app_desc_t`, and,
  on the Xtensa chips, seeds functions from every aligned `entry`
  instruction so that callbacks and tasks reached only through pointers are
  found. On the RISC-V chips it finds ESP-IDF's interrupt vector table
  (`_vector_table`, or the CLIC's `_mtvt_table` on the ESP32-C5) and names
  and seeds its handlers, and reads the value `call_start_cpu0` loads into
  `gp` (`la gp, __global_pointer$`) so that `gp`-relative accesses to small
  data resolve from the start. The value is set as the view's global
  pointer value (*user* value, since Binary Ninja only allows setting those),
  once, when the file is first loaded: one you set yourself is left alone,
  and one you change or clear stays that way when the database is reopened.
  Bootloader images do not use `gp`. On the ESP32-C6, H2 and C5, where SRAM
  is one region seen by both buses and flash is mapped through one window
  shared by code and read-only data, the SRAM segments are mapped writable
  and executable and the flash window read-only, as code.
- **Peripheral registers from SVD**: Espressif's SVD files for the ESP32,
  ESP32-S2, ESP32-S3, ESP32-C2, ESP32-C3, ESP32-C6 and ESP32-H2
  (https://github.com/espressif/svd, Apache-2.0) are
  bundled (the `svd` git submodule) and applied automatically to ESP
  firmware images and ELF files by Binary Ninja's SVD loader, naming and typing every
  peripheral register (`UART0.INT_CLR.RXFIFO_FULL_INT_CLR = 9`). An SVD file chosen in *Open with
  Options* (*SVD File*, `analysis.svd.autoLoadFile`) takes precedence, and
  `espressif.svd.autoLoad` turns this off. The files are unpacked once to
  `<user directory>/espressif/svd/`. This needs Binary Ninja's SVD loader
  plugin (`svd_ninja`); without it, peripherals are left unnamed. The ESP8266
  has no official SVD, so the view names its peripheral blocks itself; there
  is no SVD file for the ESP32-C5 yet.
- **Flash dumps**: a dump with a partition table at flash offset 0x8000
  opens as a container (the `EspFlash` transform), like an archive: its
  files are `bootloader` (the second stage bootloader, up to the table),
  `partition-table`, and one per partition, named by its label, each
  described by its type, subtype, flash offset and size. In the UI,
  Binary Ninja's container browser lists them all to choose from. Headless,
  `bn.load` opens the one the `files.espressif.partition` setting names,
  by default the factory app (or else the first OTA app with an image):
  `bn.load(path, options={"files.espressif.partition": "ota_1"})`, or
  `"bootloader"`, `"nvs"`, ...; `"*"` opens the whole dump instead, as
  does a name the dump has no file for (with an error logged). The app and
  bootloader files open with the `ESP` view; data partitions and the
  partition table are not code, and are left to Binary Ninja (`bn.load`
  opens them in a `Mapped` view with its default architecture). The
  view of a file records where it came from (its virtual path, partition
  label, type, subtype, flash offset and size) in its
  `espressif.flash.entry` metadata, which a database keeps. A single
  partition, as `esptool.py read_flash` or `parttool.py read_partition`
  reads it, or an image from a build, has no partition table: it is not a
  container, and opens directly with the `ESP` view.

  With the container off (`files.container.mode` set to `Disabled`, or
  `files.container.excludedTransforms` containing `EspFlash`), the `ESP`
  view loads the whole dump, mapping only one image of it: the app
  partition or bootloader `files.espressif.partition` names, by default the
  factory app. There it is read from the global settings (`bn.Settings()`):
  in `bn.load`'s options it only reaches the container. The files of a dump can also be listed and extracted with
  `binaryninja.transform.TransformSession` (with
  `options={"files.espressif.partition": "*"}` to see them all, as in
  `tests/test_headless.py`); keep the session alive as long as views of its
  files are open.
- **ELF support**: `EM_XTENSA` and `EM_RISCV` files open with the ELF view
  on the chip's platform. A RISC-V file counts as Espressif firmware only if
  it is 32-bit, uses the soft-float ABI and loads entirely into one
  supported chip's memory; any other RISC-V file keeps Binary Ninja's own
  platform (`linux-rv32gc`). The chip is the one whose memory map holds
  every loaded segment. Where several do (the C2 and C3; the C6, H2 and C5;
  some Xtensa layouts), the `ABS` symbols ESP-IDF puts in the ELF for the ROM
  functions it calls decide: the chip whose ROM has the most of them, by
  name and address. Failing that, the load addresses (and, for Xtensa,
  whether the entry point uses the windowed ABI) pick the most likely chip.
  To force a chip, set Binary Ninja's standard `loader.platform` load
  setting to its platform (`esp32`, `esp32c3`, ...):
  *Open with Options* → *Platform*, or
  `bn.load(path, options={"loader.platform": "esp32s3"})`. It takes
  precedence over the recognised chip.
- **ESP-IDF ELF files** analyze like firmware images: sections in the
  chip's flash data (`.flash.rodata`, which ESP-IDF marks writable) are made
  read-only data, so jump tables and constants in them resolve; the mask
  ROM's functions and data are named from the ELF's `ABS` symbols, and from
  the bundled ROM table for names it lacks (the symbols of every chip
  revision only, as an ELF does not say which revision it needs), and
  typed; and the RISC-V global pointer is taken from `__global_pointer$`.
- **Mask ROM types**: the ROM functions get their prototypes, and the ROM
  variables their types, so that calls into the ROM decompile with the
  right arguments and results (`__udivdi3(a, b)` on two `uint64_t`s rather
  than four registers, `ets_delay_us(n)`, `memset(&buf, c, n)`) and ROM
  data shows its fields (`rom_spiflash_legacy_data->chip.chip_size`). Every
  chip but the ESP8266 has a type library, generated from ESP-IDF v5.4.1's
  ROM headers (`components/esp_rom`), newlib's headers for the C library
  functions in the ROM, and the libgcc helpers' standard prototypes
  (`scripts/rom_types_extra.h`), and embedded in the plugin; it is unpacked
  to `<user directory>/espressif/rom_types/` at startup. The ROM's code is
  mapped as a memory region of its own (`espressif.rom`), holding the ROM
  image if there is one (see **Mask ROM images** below) and zeros
  otherwise, where each typed ROM function is defined as an imported
  function: Binary Ninja applies a callee's type only to a function, and
  creates functions only where there are bytes. ROM functions with several names (`ets_printf` and
  `esp_rom_printf`) are typed under the one already shown, and revision-
  specific ones (the ESP32 v3.0 ROM's Secure Boot v2 API) only for images
  that require that revision. All of the ROM functions that ESP-IDF's
  `hello_world` calls are typed (ESP32 and ESP32-C3); of all the names in
  the ROM tables, from 14% (ESP32-C2) to 66% (ESP32-H2) are, as most of
  the rest (Bluetooth, Wi-Fi and PHY internals) have no public header:

  | Chip     | Typed ROM functions | Variables | Library |
  |----------|---------------------|-----------|---------|
  | ESP32    | 455 of 1736 (26%)   | 11        | 26 KiB  |
  | ESP32-S2 | 440 of 863 (51%)    | 4         | 26 KiB  |
  | ESP32-S3 | 465 of 1967 (24%)   | 7         | 30 KiB  |
  | ESP32-C2 | 365 of 2544 (14%)   | 6         | 25 KiB  |
  | ESP32-C3 | 388 of 1908 (20%)   | 6         | 27 KiB  |
  | ESP32-C6 | 376 of 1117 (34%)   | 6         | 26 KiB  |
  | ESP32-H2 | 363 of 553 (66%)    | 6         | 25 KiB  |
  | ESP32-C5 | 381 of 1338 (28%)   | 6         | 26 KiB  |

  Databases keep the types; a database saved before this feature existed is
  not retyped when reopened.
- **Mask ROM images**: the ROM region holds the chip's real ROM when one of
  Espressif's ROM ELF files is available, so the ROM's code can be read
  and decompiled (`memset` at 0x4000c44c on the ESP32 starts
  `entry a1, 16`). The plugin does not bundle them: they come from
  [esp-rom-elfs](https://github.com/espressif/esp-rom-elfs), whose ELF
  files are licensed under Apache-2.0; the ROM code itself is closed
  source, Copyright Espressif Systems, and includes third-party libraries
  listed in [ESP-IDF's copyrights page](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/COPYRIGHT.html#rom-source-code-copyrights).
  The plugin looks for `<chip>_rev<REV>_rom.elf`, in order, in:
  1. the directory in the `espressif.rom.elfDirectory` setting;
  2. ESP-IDF's tools install, `$IDF_TOOLS_PATH/tools/esp-rom-elfs/<version>/`
     and `~/.espressif/tools/esp-rom-elfs/<version>/` (newest version
     first);
  3. its own download cache, `<user directory>/espressif/rom-elfs/<release>/`.

  The first file that is the chip's (an ELF of the chip's architecture
  whose symbols put `ets_printf` and `memset`, or their ROM jump table
  entries, where the chip's ROM table does) is used, and only its loadable
  contents within the ROM's code range. The revision is the minimum chip
  revision an ESP image requires, and the lowest for an ELF file, or the
  `espressif.rom.revision` setting's (`v3.0`; settable per file with
  *Open with Options*); the file of the highest revision up to it is used,
  or the lowest there is. `espressif.rom.loadImages` turns all this off.

  **Downloading** happens only with your consent, from the esp-rom-elfs
  release pinned in `src/espressif/rom_images.rs` (20260528, 4.9 MB),
  over HTTPS through Binary Ninja's download provider (its proxy settings
  apply), never while a file loads (the ROM region holds zeros meanwhile):
  - In the UI, the first file that would use a ROM image asks once whether
    to download them, naming the source and licence. If you answer No,
    it doesn't ask again for that release. If you answer Yes, the image is
    mapped into that file when the download is done; other open files map
    it when they are next reanalyzed or reopened.
  - **Plugins > Espressif > Download ROM Images** downloads them on
    request and maps the image into the current file, database or not.
  - Headless, it never asks; it downloads only if the
    `espressif.rom.autoDownload` setting is enabled (with it on, the UI
    doesn't ask either). A script then reanalyzes or reopens its views.

  The archive is used only if its size and SHA-256 match the pinned ones;
  only the ROM ELF files of the chips the plugin supports are unpacked,
  under names made from the chip and revision, never from the archive's
  paths.

  With real code in the ROM, Binary Ninja analyzes the typed ROM
  functions' bodies, and creates the ROM functions they call (about 80
  more on the ESP32, 450 to 600 on the RISC-V chips and the ESP32-S3, whose
  ROM API is a jump table to the implementations), not the whole ROM: the
  region is not swept. The ROM's other functions are named from the ELF
  file's symbol table. The typed ROM functions' names, prototypes and
  purity are made user data then, as the analysis of a body would change
  them (a jump table entry is named `j_sub_...` after its target, `void *`
  parameters narrow to `uint8_t *`, `__pure` is inferred), and the
  functions behind the jump table get the same prototypes. They still
  clobber the caller-saved registers, the API's contract, whatever their
  bodies do. A database keeps the ROM region it was saved with: one saved
  with zeros gets the image only through **Download ROM Images**. The ROM
  adds a few MB to a database (the ROM image and the analysis of its
  functions: 2.2 MB to 4.3 MB for an ESP32 test ELF).
- **Relocatable objects**: `.o` files, and the members of static libraries
  such as ESP-IDF's binary Wi-Fi, PHY and Bluetooth libraries
  (`libnet80211.a`, `libphy.a`, `libbtdm_app.a`), load with their
  relocations applied by an ELF relocation handler on each Xtensa
  architecture: `R_XTENSA_32` (literal pools and data pointers, adding to
  the part of the addend the assembler leaves in the word),
  `R_XTENSA_32_PCREL`, and `R_XTENSA_SLOT0_OP` on `l32r`, the calls, `j`,
  the conditional branches (`beqz.n`/`bnez.n` included), the
  zero-overhead loops and `movi`. Every re-encoded instruction is decoded
  to check it; one that cannot reach its target (out of range, misaligned,
  or an `l32r` whose literal lies after it) is left as assembled and a
  warning is logged. The linker's relaxation hints (`ASM_EXPAND`,
  `ASM_SIMPLIFY`) and label differences (`DIFF`, `PDIFF`, `NDIFF`, already
  stored by the assembler) need nothing. Binary Ninja places an object's
  sections one after another from 0x10000, in section header order, and
  GNU as puts the literal sections first, so `l32r` reaches its literals;
  undefined symbols become external symbols. An object has no addresses to
  tell the chips apart by, so call0 objects (`ABI=1` in their
  `.xtensa.info`) open as `esp8266` and windowed ones as `esp32`: set
  `loader.platform` for ESP32-S2 or ESP32-S3 code. `bn.load` does not open
  a `.a` archive; Binary Ninja's `AR` container transform lists its
  members (headless, through `binaryninja.transform.TransformSession`, as
  in `tests/test_headless.py`), or extract them with `ar x`.

Assembling is not supported on the Xtensa architectures.

Not supported for now:

- **ESP32-P4.** Its images are not loaded, and its ELFs are not recognised
  as Espressif firmware. Its custom instructions (the PIE SIMD extension,
  in two incompatible versions for chip revisions before and from v3.0, and
  the hardware loops) are not decoded; Binary Ninja's `rv32gc` treats them
  as invalid instructions.
- **Other relocations.** The dynamic and TLS relocations (Espressif
  firmware is linked statically), the FLIX slot relocations
  (`R_XTENSA_SLOTn_OP`/`ALT` for slots 1 and up, and `SLOT0_ALT`) and
  `const16` operands are not applied; Espressif's cores have neither FLIX
  nor `const16`. Binary Ninja's DWARF import rejects the Xtensa relocations
  of an object's debug sections, so objects built with `-g` load without
  their debug information.
- **ESP8266 ROM types.** Its ROM functions are named but not typed: the
  ESP-IDF docker image the type libraries are made from has no ESP8266 SDK
  headers.
- **Zcb/Zcmp on the ESP32-C5.** ESP-IDF only emits them with
  `CONFIG_COMPILER_ENABLE_RISCV_ZCMP` (off by default). `rv32gc` decodes
  `cm.push`/`cm.pop` as `fsd` and does not decode the Zcb instructions, so
  firmware built with that option analyzes incorrectly.

## Installing

The plugin is a native library, but Binary Ninja's extension manager only
installs Python plugins, from the source of the latest release. So what it
installs is a small Python loader (`__init__.py` and
`binja_xtensa_loader/`), which installs the prebuilt native library from
the matching GitHub release into your
[user plugin folder](https://docs.binary.ninja/guide/index.html#user-folder).
Prebuilt libraries exist for Linux x86_64, Windows x86_64 and macOS
(universal), built for the core ABI of the stable Binary Ninja named in
`scripts/use_stable_api.sh`. For any other platform, or a dev build of
Binary Ninja whose core ABI differs, the loader logs what is available and
does nothing; [build from source](#building-from-source) instead.

1. Install **Espressif Xtensa** in the extension manager.
2. Nothing is downloaded without your consent:
   - In the UI, the loader asks once whether to download the library (it
     asks again only when a new version needs a different library). If you
     decline, use **Plugins > Espressif > Install or Update Native Library**
     later.
   - Headless, it never asks; it downloads only if the
     `espressif.loader.autoDownload` setting is enabled (in the UI, or in
     `settings.json` in the user folder:
     `{"espressif.loader.autoDownload": true}`), and otherwise logs what to
     do. With the setting on, the UI doesn't ask either.
3. Restart Binary Ninja to load the library. The loader does not load it
   from Python: Binary Ninja loads it like any native plugin, after the
   plugins it depends on.

The library is downloaded over HTTPS from this repository's GitHub release
(redirects are only followed to GitHub's release asset storage), and is
installed only if its size and SHA-256 match
`binja_xtensa_loader/manifest.json`, which is part of the plugin's source as
the extension manager installed it; nothing fetched from the release is
trusted. It is written to a temporary file in the plugin folder, checked,
and then renamed to `binja_xtensa-native.so` (`.dll`, `.dylib`), so a new
version replaces the old one and only one is ever installed.

If the plugin folder already has a copy of the plugin, such as a
`libbinja_xtensa.so` you built (any `*binja_xtensa*` library), the loader
doesn't install a second one, which would register the same architectures
twice, and logs why. If the native plugin is already loaded from elsewhere,
it doesn't ask.

**Uninstalling.** Removing the plugin in the extension manager removes the
loader but not the native library. Run **Plugins > Espressif > Remove
Native Library** first (it then doesn't ask about that version again, and
turns `espressif.loader.autoDownload` off if it was on), or delete
`binja_xtensa-native.so` (`.dll`, `.dylib`) from the user plugin folder
yourself, then restart Binary Ninja.

## Building from source

Requires a Rust toolchain at least as new as the binaryninja-api revision's
`rust-version` (1.98.1 at the pinned revision), libclang for `bindgen`
(`libclang-dev` on Debian/Ubuntu), and a Binary Ninja installation whose core
matches the API revision pinned in `Cargo.toml`
(`binaryninjacore-sys` finds the installation through
`~/.binaryninja/lastrun` or `~/binaryninja`; set `BINARYNINJADIR` to
override).

```sh
git submodule update --init      # Espressif's SVD files (or clone with --recursive)
cargo build --release
cp target/release/libbinja_xtensa.so ~/.binaryninja/plugins/   # .dylib / .dll elsewhere
```

To pick up newer SVD files, `git submodule update --remote svd` and rebuild;
`build.rs` compresses the files of the supported chips into the plugin.

A native plugin loads only in a Binary Ninja whose core ABI matches the API
it was built against (`BN_MINIMUM_CORE_ABI_VERSION` to
`BN_CURRENT_CORE_ABI_VERSION` in `binaryninjacore.h`): a build for a 6.0
stable core (ABI 187) is refused by a 6.1 dev core (ABI 188-189), and the
other way round. `Cargo.toml` on `main` pins a `dev` revision of the API, so
a plain `cargo build` is for a dev Binary Ninja. Releases are built against
the stable tag named in `scripts/use_stable_api.sh`, which rewrites both pins
in `Cargo.toml` to that tag and updates `Cargo.lock`; run it (and don't
commit the result) to build for a stable Binary Ninja:

```sh
scripts/use_stable_api.sh
cargo build --release
```

CI builds and tests against both. To move to a newer dev revision, update
the `rev` of `binaryninja` and `binaryninjacore-sys` in `Cargo.toml`; to
move to a newer stable release, update `STABLE_TAG` in
`scripts/use_stable_api.sh`.

A self-built library in the plugin folder takes precedence: if the
extension manager's loader is installed too, it sees the library and
installs nothing. Don't copy a self-built library into the plugin folder
next to an installed `binja_xtensa-native.so`; remove that first.

### Regenerating the ROM tables and types

`src/espressif/rom_symbols.rs` is generated from the ROM linker scripts by
`scripts/gen_rom_symbols.py` (see its header for the sources). The ROM type
libraries in `data/rom_types/` are generated, offline, by
`scripts/gen_rom_types.py`, which runs headless Binary Ninja with a plugin
build (for the chip platforms) and parses the headers of the ESP-IDF version
named in it, copied out of the `espressif/idf` docker image by
`scripts/fetch_rom_type_headers.sh`:

```sh
cargo build --release
scripts/fetch_rom_type_headers.sh /tmp/rom-headers
scripts/gen_rom_types.py /tmp/rom-headers esp32 esp32s2 esp32s3 esp32c2 esp32c3 esp32c6 esp32h2 esp32c5
cargo build --release   # embeds the new libraries
```

Each library records the ESP-IDF version and the list of headers it was
made from in its metadata (`espressif.idf_version`, `espressif.headers`):
the chip's `components/esp_rom/<chip>/include/<chip>/rom/*.h`, esp_rom's
chip-neutral `components/esp_rom/include/*.h`, `xtensa/xtruntime.h` and
`xtensa/hal.h` (Xtensa) or `riscv/interrupt.h` (RISC-V), newlib's
`string.h`, `strings.h`, `stdio.h`, `stdlib.h`, `ctype.h`, `time.h`,
`sys/unistd.h`, `sys/_default_fcntl.h`, `sys/lock.h`, `reent.h`,
`sys/reent.h`, `setjmp.h`, `locale.h`, `sys/time.h`, `sys/stat.h`,
`signal.h`, `assert.h`, `envlock.h` and `malloc.h`, and
`scripts/rom_types_extra.h`.

## Releasing

The loader trusts only the hashes committed in the source the extension
manager installs, that is, in the release tag. A tag cannot contain the
hashes of libraries built from itself, so a release is built from one
commit and published from a tag on a commit that adds only the manifest,
by `.github/workflows/release.yml` (manual only) in two runs:

1. **Build.** Run the Release workflow on `main` (or another branch) with
   mode `build`, and optionally the version being released (default:
   `plugin.json`'s):

   ```sh
   gh workflow run release.yml --ref main -f mode=build -f version=0.2.0
   ```

   It builds the dispatched commit for Linux, Windows and macOS against the
   stable API, uploads the libraries as artifacts of the run (kept 90 days),
   and generates the loader's manifest: the version, the build commit, the
   run ID, and each library's name, core ABI, size and SHA-256. The run
   summary shows it, and the `manifest` artifact holds it.
2. **Commit and tag.** On top of the build commit, save the manifest as
   `binja_xtensa_loader/manifest.json`, set `plugin.json`'s `version` to
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

`scripts/release_manifest.py` does the generating and checking. Between
releases, `main`'s manifest stays that of the last release; if
`plugin.json`'s version moves ahead of it, the loader of a checkout of
`main` refuses to install anything, as it isn't a release.

## Testing

```sh
cargo test --lib --no-default-features    # image formats, partitions, loops, intrinsics, patching, relocations
cargo test --release --test lift_sweep    # lifts ~1.5M encodings through the real core
cargo build --release && tests/test_headless.py
tests/test_loader.py                      # the Python loader and release tooling
cargo build --release && tests/test_loader_headless.py
cargo test -p xtensa                      # the decoder (see xtensa/README.md)
```

The unit tests link a stub core generated by `binaryninjacore-sys` (which
needs CMake 3.28+ and Ninja), so they need no Binary Ninja installation.
`lift_sweep` and the `test_headless` scripts need a headless-capable Binary
Ninja license; `lift_sweep` skips itself when the core cannot be
initialized. `tests/test_headless.py`
runs Binary Ninja with a throwaway user directory containing only this plugin
and checks ELF loading, chip detection, calling conventions, call-site
arguments, loop recovery, lifting semantics, patching, and the ESP view
(including partition selection and malformed images) and the flash dump
container on images and flash dumps synthesized from `tests/data/*.elf`.
Those ELFs are built from
`tests/data/corpus.c` by `scripts/build_test_elfs.sh` with Espressif's GCC,
which also builds the relocatable objects in `tests/data/obj` (with and
without `-mlongcalls`, ESP-IDF style with `-ffunction-sections`, and
`tests/data/reloc_edge.S`, whose relocations cannot all be applied) and
lists their relocations with objdump: the test checks every
`R_XTENSA_SLOT0_OP` and `R_XTENSA_32` Binary Ninja applies against that
listing. `tests/data/idf_*.elf`, built from `tests/data/idf_features.c` by
`scripts/build_idf_elfs.sh`, are laid out like ESP-IDF applications and
call into the mask ROM: the test checks the ELF fix-ups and the ROM types
(the arguments of ROM calls and libgcc helpers, a ROM structure's fields,
and that reopening a database changes nothing) on them and on images made
from them. The user directory (`bn-xtensa-test-*` in the system temp
directory) also holds the databases and images the test writes, and is
removed when the test exits; set `BN_XTENSA_KEEP_TMP=1` to keep it for
debugging (the test prints its path).

Those checks run without ROM images (ESP-IDF's are hidden from the plugin).
The mask ROM image checks need the pinned esp-rom-elfs archive, which the
test does not download:

```sh
curl -LO https://github.com/espressif/esp-rom-elfs/releases/download/20260528/esp-rom-elfs-20260528.tar.gz
BN_XTENSA_ROM_TARBALL=$PWD/esp-rom-elfs-20260528.tar.gz tests/test_headless.py
```

They check the ROM types again with a directory of ROM ELF files (and in
databases), the revision chosen, a view that maps an image found after it
was loaded, and the download: served from a local HTTP server (through the
`BN_XTENSA_ROM_TEST_URL` override, loopback only), not without consent, not
when corrupted, and through the command.

`tests/test_loader.py` needs only Python (it stands in for the
`binaryninja` module where it needs one) and runs in CI.
`tests/test_loader_headless.py` installs the loader into a throwaway user
directory with a manifest listing the locally built library as the release
asset for the running core's ABI, serves it from a local HTTP server (the
loader fetches from `$BN_XTENSA_LOADER_TEST_BASE_URL` instead of GitHub; it
accepts only loopback addresses there, and still checks the manifest's hash),
and runs headless Binary Ninja several times: it checks that nothing is
downloaded without `espressif.loader.autoDownload`, that with it the library
lands in the user plugin folder as `binja_xtensa-native.so` and loads after
a restart, and that a tampered asset or a self-built copy in the plugin
folder means nothing is installed.

## License

MIT, see [LICENSE](LICENSE). The Espressif SVD files embedded in the plugin
(from the `svd` submodule) are licensed under Apache-2.0 by Espressif. The
mask ROM symbol tables are derived from ESP-IDF's ROM linker scripts
(Apache-2.0, Espressif) and the ESP8266 RTOS and NONOS SDKs' (Apache-2.0
and the Espressif MIT License). The ROM type libraries in `data/rom_types/`
are derived from ESP-IDF v5.4.1's headers (Apache-2.0, Espressif) and from
newlib's headers as shipped with Espressif's toolchains (BSD-style licenses,
see newlib's `COPYING.NEWLIB`). The mask ROM images are not part of the
plugin: they are downloaded, with your consent, from
[esp-rom-elfs](https://github.com/espressif/esp-rom-elfs), whose ELF files
Espressif licenses under Apache-2.0; the ROM code is Copyright Espressif
Systems, not open source, and includes the third-party libraries listed
[here](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/COPYRIGHT.html#rom-source-code-copyrights).
