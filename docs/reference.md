# Technical reference

How binja-espressif behaves in detail: what it registers, how it loads each
kind of file, and how the ROM support works. The [README](../README.md)
covers installing and everyday use; [development.md](development.md)
covers building and testing.

- [Architectures and platforms](#architectures-and-platforms)
- [Calling conventions](#calling-conventions)
- [Loops, stack frames and patching](#loops-stack-frames-and-patching)
- [The ESP view](#the-esp-view)
- [Flash dumps](#flash-dumps)
- [ELF files](#elf-files)
- [Relocatable objects](#relocatable-objects)
- [Peripheral registers](#peripheral-registers)
- [Mask ROM types](#mask-rom-types)
- [Mask ROM images](#mask-rom-images)
- [The native library loader](#the-native-library-loader)
- [Limitations](#limitations)

## Architectures and platforms

- **Architectures** `xtensa-esp8266`, `xtensa-esp32`, `xtensa-esp32s2` and
  `xtensa-esp32s3`, each decoding exactly the instructions its core
  implements (including MAC16, booleans, FPU, the ESP32 DFP assist, the S2
  dedicated-GPIO TIE and the S3 PIE vector extension), and a standalone
  platform of the same name. Decoding is done by the [`xtensa`](../xtensa/)
  crate, which is verified against GNU objdump.
- **Platforms** `esp8266`, `esp32`, `esp32s2`, `esp32s3`, `esp32c2`,
  `esp32c3`, `esp32c6`, `esp32h2` and `esp32c5` (the ESP-IDF target names,
  under the OS `espressif`), one per chip, on the chip's architecture and
  with its calling conventions. ESP images and ELF files open with these;
  the chip, for example for the SVD file, is taken from the platform. The
  architectures' standalone platforms remain available (e.g. for
  `loader.platform`), but do not identify a chip. The RISC-V chips use
  Binary Ninja's own `rv32gc` architecture.
- **Disassembly** in GNU objdump syntax, with branch/call targets and `l32r`
  literals as address tokens.
- **Lifting** of the whole ISA to LLIL: every decodable instruction lifts
  (checked by `arch_xtensa`'s `liftcheck` lift sweep), with native semantics
  for the integer, load/store, branch, call, shift/SAR, zero-overhead loop,
  boolean, floating point and conditional-move instructions, and intrinsics
  for the rest.
- **Flags**: the boolean registers `b0`–`b15` (Xtensa has no condition
  codes).
- **Intrinsics** named after their mnemonics (`__memw`, `__rsil`,
  `__ee_vmulas_s16_accx_ld_ip`, ...) and for accesses to hardware special
  and user registers (`__rsr_ccount()`, `__wsr_intenable(x)`,
  `__xsr_vecbase(x)`). Each intrinsic's inputs and outputs are derived from
  an exemplar encoding of its opcode (`arch/xtensa/src/exemplars.rs`, see
  [development.md](development.md#exemplars)).

Assembling is not supported on the Xtensa architectures.

## Calling conventions

- `windowed` (the `call8` ABI used for all ESP32 family code; the default
  there) and `call0` (the ESP8266 ABI, and hand-written or `-mabi=call0`
  code on the others).
- Functions that do not begin with `entry` on the windowed cores get
  `call0` before their parameters and return value are inferred, unless you
  have given the function a type or calling convention yourself (for
  example a windowed stub with no `entry`, typed `__convention("windowed")`).
- There, `callx0` sites are typed as `call0` calls (with the callee's type,
  or variadic for an unknown target), since Binary Ninja otherwise calls
  through a register with the platform's default convention; a call site
  type you set is kept, and so is the type of a function pointer called
  through (give it `__convention("call0")`).
- Both place arguments as GCC does: in the six argument registers, a value
  aligned to 8 bytes (`double`, 64-bit integers) starting at an even one and
  structures included, and on the stack from the first argument that does
  not fit.
- Functions whose type was imported from a type library (such as the ROM
  functions) keep the convention and clobbers their type says.
- On `rv32gc`, `ilp32` (from the `arch_riscv_ilp32` crate; see its
  [README](../arch/riscv_ilp32/README.md)) is the RISC-V chip platforms'
  default: the soft-float ABI ESP-IDF builds those chips for. The
  architecture's own `default` passes floating point values in `fa0`–`fa7`.

## Loops, stack frames and patching

- **Zero-overhead loops** (`loop`/`loopnez`/`loopgtz`) are recovered as
  real loops.
- **Windowed stack frames**: the register save areas at the top of an
  `entry` frame (32 bytes, as GCC lays it out) end the function's locals,
  so a local whose address is passed on keeps its size instead of growing
  into an array that reaches the top of the frame. `entry` reads the bottom
  word of the save areas into an unused temporary for this (`temp0 =
  var_20` in the MLIL); the read is not one the processor makes, and dead
  code elimination leaves it out of the HLIL.
- **Patching** on the Xtensa architectures: converting to nops (`nop`,
  `nop.n`), never branch, invert branch (every conditional branch, narrow
  `beqz.n`/`bnez.n` included) and always branch (a 3-byte branch becomes a
  `j` to the same target; a narrow branch has no room for one). Skip and
  return value turns a call into a `movi` of the value (-2048 to 2047) to
  the register the call returns in: `a2` after `call0`, `a10` after
  `call8`. Loop instructions and `entry` are not offered as branches.

## The ESP view

The `ESP` binary view loads raw firmware:

- ESP32/ESP32-S2/ESP32-S3 and ESP32-C2/C3/C6/H2/C5 application and
  bootloader images from any ESP-IDF version (the chip is taken from the
  extended header's `chip_id`; images from before that field existed are
  recognised as ESP32);
- ESP8266 v1 images (NONOS/RTOS SDK, including the RTOS SDK v3 layout with
  an extended header) and v2 images (`0xEA`, NONOS OTA `user1.bin`,
  Arduino);
- flash dumps, starting at flash offset 0 or 0x1000 (as `esptool merge_bin`
  and most firmware distributions produce for the ESP32/ESP32-S2), through
  the `EspFlash` container transform (see [Flash dumps](#flash-dumps)).
  ESP8266 NONOS dumps (no partition table: a v1 image at 0 that maps no
  flash itself, with data written past it from 0x10000) are mapped as a
  whole through the 0x40200000 flash cache window so `irom0` code is
  present. An ESP8266 app on its own, however long, is not.

The view maps every segment into the chip's memory map with appropriate
permissions and section semantics, and adds unbacked segments for `.bss`,
RTC memory and the peripheral register blocks. It names the mask ROM's
functions and data (from the ESP-IDF and ESP8266 SDK linker scripts) and
types them (see [Mask ROM types](#mask-rom-types)), names the ESP8266's
peripheral blocks as its SDK's linker script does (`uart0`, `frc1`: aliases
of the SVD file's names), and types the ESP-IDF `esp_app_desc_t`.

- On the Xtensa chips it seeds functions from every aligned `entry`
  instruction, so that callbacks and tasks reached only through pointers
  are found.
- On the RISC-V chips it finds ESP-IDF's interrupt vector table
  (`_vector_table`, or the CLIC's `_mtvt_table` on the ESP32-C5) and names
  and seeds its handlers, and reads the value `call_start_cpu0` loads into
  `gp` (`la gp, __global_pointer$`) so that `gp`-relative accesses to small
  data resolve from the start. The value is set as the view's global
  pointer value (a *user* value, since Binary Ninja only allows setting
  those), once, when the file is first loaded: one you set yourself is left
  alone, and one you change or clear stays that way when the database is
  reopened. Bootloader images do not use `gp`.
- On the ESP32-C6, H2 and C5, where SRAM is one region seen by both buses
  and flash is mapped through one window shared by code and read-only data,
  the SRAM segments are mapped writable and executable and the flash window
  read-only, as code.

## Flash dumps

A dump with a partition table at flash offset 0x8000 opens as a container
(the `EspFlash` transform), like an archive. Its files are `bootloader`
(the second stage bootloader, up to the table), `partition-table`, and one
per partition, named by its label, each described by its type, subtype,
flash offset and size.

- In the UI, Binary Ninja's container browser lists them all to choose
  from.
- Headless, `bn.load` opens the one the `files.espressif.partition` setting
  names, by default the factory app (or else the first OTA app with an
  image): `bn.load(path, options={"files.espressif.partition": "ota_1"})`,
  or `"bootloader"`, `"nvs"`, ...; `"*"` opens the whole dump instead, as
  does a name the dump has no file for (with an error logged).

The app and bootloader files open with the `ESP` view; data partitions and
the partition table are not code, and are left to Binary Ninja (`bn.load`
opens them in a `Mapped` view with its default architecture). The view of a
file records where it came from (its virtual path, partition label, type,
subtype, flash offset and size) in its `espressif.flash.entry` metadata,
which a database keeps.

A single partition, as `esptool.py read_flash` or `parttool.py
read_partition` reads it, or an image from a build, has no partition table:
it is not a container, and opens directly with the `ESP` view.

With the container off (`files.container.mode` set to `Disabled`, or
`files.container.excludedTransforms` containing `EspFlash`), the `ESP` view
loads the whole dump, mapping only one image of it: the app partition or
bootloader `files.espressif.partition` names, by default the factory app.
There it is read from the global settings (`bn.Settings()`): in `bn.load`'s
options it only reaches the container.

The files of a dump can also be listed and extracted with
`binaryninja.transform.TransformSession` (with
`options={"files.espressif.partition": "*"}` to see them all, as in
`tests/test_headless.py`); keep the session alive as long as views of its
files are open.

## ELF files

`EM_XTENSA` and `EM_RISCV` files open with the ELF view on the chip's
platform. A RISC-V file counts as Espressif firmware only if it is 32-bit,
uses the soft-float ABI and loads entirely into one supported chip's
memory; any other RISC-V file keeps Binary Ninja's own platform
(`linux-rv32gc`).

The chip is the one whose memory map holds every loaded segment. Where
several do (the C2 and C3; the C6, H2 and C5; some Xtensa layouts), the
`ABS` symbols ESP-IDF puts in the ELF for the ROM functions it calls
decide: the chip whose ROM has the most of them, by name and address.
Failing that, the load addresses (and, for Xtensa, whether the entry point
uses the windowed ABI) pick the most likely chip. To force a chip, set
Binary Ninja's standard `loader.platform` load setting to its platform
(`esp32`, `esp32c3`, ...): *Open with Options* → *Platform*, or
`bn.load(path, options={"loader.platform": "esp32s3"})`. It takes
precedence over the recognised chip.

**ESP-IDF ELF files** analyze like firmware images (the
`espressif.elf.fixups` analysis activity):

- On the C6, H2 and C5, whose code and data share one flash window, ESP-IDF
  (v5.x and v6.1) starts the rodata's PT_LOAD at the code's address with
  the zeros of `.flash_rodata_dummy`, a `NOLOAD` placeholder the size of the
  code, and Binary Ninja maps that PT_LOAD over the code's. The code is
  mapped from the file again, and placeholder sections (also the S3's
  `.ext_ram.dummy` over `.flash.rodata`) are trimmed off the code and data
  they overlap, so that these are not writable to analysis.
- Sections in the chip's flash data (`.flash.rodata`, which ESP-IDF marks
  writable) are made read-only data, so jump tables and constants in them
  resolve.
- The mask ROM's functions and data are named from the ELF's `ABS`
  symbols, and from the bundled ROM table for names it lacks (the symbols
  of every chip revision only, as an ELF does not say which revision it
  needs), and typed.
- The RISC-V global pointer is taken from `__global_pointer$`.

**Espressif's ROM ELF files** (from esp-rom-elfs), opened directly, get
their own chip: the ROM functions are typed and pinned as in a ROM image,
the file maps the ROM itself, and its `espressif.rom.elf` metadata names
the chip.

## Relocatable objects

`.o` files, and the members of static libraries such as ESP-IDF's binary
Wi-Fi, PHY and Bluetooth libraries (`libnet80211.a`, `libphy.a`,
`libbtdm_app.a`), load with their relocations applied by an ELF relocation
handler on each Xtensa architecture:

- `R_XTENSA_32` (literal pools and data pointers, adding to the part of the
  addend the assembler leaves in the word), `R_XTENSA_32_PCREL`, and
  `R_XTENSA_SLOT0_OP` on `l32r`, the calls, `j`, the conditional branches
  (`beqz.n`/`bnez.n` included), the zero-overhead loops and `movi`.
- Every re-encoded instruction is decoded to check it; one that cannot
  reach its target (out of range, misaligned, or an `l32r` whose literal
  lies after it) is left as assembled and a warning is logged.
- The linker's relaxation hints (`ASM_EXPAND`, `ASM_SIMPLIFY`) and label
  differences (`DIFF`, `PDIFF`, `NDIFF`, already stored by the assembler)
  need nothing.

Binary Ninja places an object's sections one after another from 0x10000, in
section header order, and GNU as puts the literal sections first, so `l32r`
reaches its literals; undefined symbols become external symbols. An object
has no addresses to tell the chips apart by, so call0 objects (`ABI=1` in
their `.xtensa.info`) open as `esp8266` and windowed ones as `esp32`: set
`loader.platform` for ESP32-S2 or ESP32-S3 code. `bn.load` does not open a
`.a` archive; Binary Ninja's `AR` container transform lists its members
(headless, through `binaryninja.transform.TransformSession`, as in
`tests/test_headless.py`), or extract them with `ar x`.

## Peripheral registers

Espressif's SVD files for the ESP32, ESP32-S2, ESP32-S3, ESP32-C2,
ESP32-C3, ESP32-C6, ESP32-H2 and ESP32-C5, and for the ESP8266 a file made
from esp-rs/esp-pacs' (see [view/espressif/svd/README.md](../view/espressif/svd/README.md)),
are bundled and applied automatically to ESP firmware images and ELF files
by Binary Ninja's SVD loader (the `espressif.svd.select` analysis activity
points it at the chip's file), naming and typing every peripheral register
(`UART0.INT_CLR.RXFIFO_FULL_INT_CLR = 9`).

- An SVD file chosen in *Open with Options* (*SVD File*,
  `analysis.svd.autoLoadFile`) takes precedence, and `espressif.svd.autoLoad`
  turns this off.
- The files are unpacked once to `<user directory>/espressif/svd/`.
- This needs Binary Ninja's SVD loader plugin (`svd_ninja`); without it,
  peripherals are left unnamed (the ESP8266's blocks keep their linker
  script names).
- The ESP8266 file covers DPORT, EFUSE, the RNG register, UART0/1, SPI0/1,
  GPIO, IO_MUX, the timers (FRC1/FRC2), RTC, WDT, SLC, I2S and the analog
  I2C master's register (`I2C_ANA`); see [Limitations](#limitations) for
  what it leaves out.

## Mask ROM types

The ROM functions get their prototypes, and the ROM variables their types,
so that calls into the ROM decompile with the right arguments and results
(`__udivdi3(a, b)` on two `uint64_t`s rather than four registers,
`ets_delay_us(n)`, `memset(&buf, c, n)`) and ROM data shows its fields
(`rom_spiflash_legacy_data->chip.chip_size`).

Every chip but the ESP8266 has a type library, generated from ESP-IDF
v6.1's ROM headers (`components/esp_rom`, and the heap component's for the
ROM's TLSF and multi_heap functions), newlib's headers for the C library
functions in the ROM, and the libgcc helpers' standard prototypes
(`scripts/rom_types_extra.h`), and embedded in the plugin; it is unpacked
to `<user directory>/espressif/rom_types/` at startup.

The ROM's code is mapped as a memory region of its own (`espressif.rom`),
holding the ROM image if there is one (see [Mask ROM images](#mask-rom-images))
and zeros otherwise, where each typed ROM function is defined as an
imported function: Binary Ninja applies a callee's type only to a
function, and creates functions only where there are bytes. ROM functions
with several names (`ets_printf` and `esp_rom_printf`) are typed under the
one already shown, and revision-specific ones (the ESP32 v3.0 ROM's Secure
Boot v2 API) only for images that require that revision.

All of the ROM functions that ESP-IDF's `hello_world` calls are typed
(ESP32 and ESP32-C3); of all the names in the ROM tables, from 15%
(ESP32-C2) to 62% (ESP32-H2) are, as most of the rest (Bluetooth, Wi-Fi and
PHY internals) have no public header:

| Chip     | Typed ROM functions | Variables | Library |
|----------|---------------------|-----------|---------|
| ESP32    | 471 of 1777 (27%)   | 11        | 27 KiB  |
| ESP32-S2 | 458 of 884 (52%)    | 4         | 26 KiB  |
| ESP32-S3 | 486 of 2035 (24%)   | 7         | 31 KiB  |
| ESP32-C2 | 388 of 2621 (15%)   | 6         | 26 KiB  |
| ESP32-C3 | 393 of 1916 (21%)   | 6         | 28 KiB  |
| ESP32-C6 | 396 of 1209 (33%)   | 6         | 27 KiB  |
| ESP32-H2 | 383 of 621 (62%)    | 6         | 25 KiB  |
| ESP32-C5 | 407 of 1411 (29%)   | 6         | 27 KiB  |

Databases keep the types. A database typed from another version of the
library (saved by an earlier build of the plugin) is brought up to date
the first time it is analyzed: the ROM functions and variables the library
now types are typed, changed prototypes and types are replaced (an edit of
yours to one of them too), and they are recorded as imported from this
library. Save the database afterwards to keep that.

## Mask ROM images

The ROM region holds the chip's real ROM when one of Espressif's ROM ELF
files is available, so the ROM's code can be read and decompiled (`memset`
at 0x4000c44c on the ESP32 starts `entry a1, 16`). The plugin does not
bundle them: they come from
[esp-rom-elfs](https://github.com/espressif/esp-rom-elfs), whose ELF files
are licensed under Apache-2.0; the ROM code itself is closed source,
Copyright Espressif Systems, and includes third-party libraries listed in
[ESP-IDF's copyrights page](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/COPYRIGHT.html#rom-source-code-copyrights).

### Finding an image

The plugin looks for `<chip>_rev<REV>_rom.elf`, in order, in:

1. the directory in the `espressif.rom.elfDirectory` setting: the file's
   own, then yours. A file gets its own when it is opened: one set with
   *Open with Options*, or headless as a load option
   (`bn.load(path, options={"espressif.rom.elfDirectory": dir})`), else a
   copy of yours; a database saves it, path included;
2. ESP-IDF's tools install, `$IDF_TOOLS_PATH/tools/esp-rom-elfs/<version>/`
   and `~/.espressif/tools/esp-rom-elfs/<version>/` (newest version first);
3. its own download cache, `<user directory>/espressif/rom-elfs/<release>/`.

The first file that is the chip's (an ELF of the chip's architecture whose
symbols put `ets_printf` and `memset`, or their ROM jump table entries,
where the chip's ROM table does) is used, and only its loadable contents
within the ROM's code range and its data bus alias. The revision is the
minimum chip revision an ESP image requires, and the lowest for an ELF
file, or the `espressif.rom.revision` setting's (`v3.0`; settable per file
with *Open with Options*); the file of the highest revision up to it is
used, or the lowest there is. `espressif.rom.loadImages` turns all this
off.

### The data bus alias

The ROM's constants (`_ctype_`, its strings, its drivers' tables) are read
by its code where the data bus sees the ROM, not where the ROM region has
them. With an image, that alias is mapped too, as read-only data
(`espressif.rom.data`), with the ELF file's `.rodata`: in
0x3FF90000..0x3FFA0000 on the ESP32, 0x3FFA0000..0x3FFB0000 on the
ESP32-S2 and from 0x3FF00000 on the ESP32-S3, C2 and C3 (the C6, H2 and C5
keep their constants in the ROM region). The ROM variables there then have
values, and the ROM code's references to them (and switch tables) resolve.
Without an image it is left unmapped, as zeros would read as the
constants' values.

### Downloading

Downloading happens only with your consent, from the esp-rom-elfs release
pinned in `view/espressif/src/rom_images.rs` (20260528, 4.9 MB), over
HTTPS through Binary Ninja's download provider (its proxy settings apply).
In the UI it never holds up loading a file (the ROM region holds zeros
meanwhile).

- In the UI, the first file that would use a ROM image asks once whether
  to download them, naming the source and licence. If you answer No, it
  doesn't ask again for that release. If you answer Yes, the image is
  mapped into that file when the download is done; other open files map it
  when they are next reanalyzed or reopened.
- **Plugins > Espressif > Download ROM Images** downloads them on request
  and maps the image into the current file, database or not. In a file
  that has the image already, it creates the ROM functions the file lacks
  (a database saved with `espressif.rom.createFunctions` off, or by
  0.1.0-rc.1, which only named most of them).
- Headless (scripts, `bn-remote`-style servers), it never asks; it
  downloads only if the `espressif.rom.autoDownload` setting is enabled
  (with it on, the UI doesn't ask either), and then the load that needs the
  image waits for the download (at most 30 s), so the view, and a database
  saved right after loading, has it. If the download fails or times out,
  the view has zeros and a warning says so; an analysis tries again a few
  minutes later. Without the setting or an image, a warning says how to get
  one.

The archive is used only if its size and SHA-256 match the pinned ones;
only the ROM ELF files of the chips the plugin supports are unpacked, under
names made from the chip and revision, never from the archive's paths.

`scripts/fetch_rom_elfs.py` does the same without Binary Ninja (Python
standard library only; run it from a checkout, as it reads the pinned
release from `rom_images.rs`), to provision a machine ahead of time: it
unpacks into the download cache of the user directory
(`$BN_USER_DIRECTORY`, else the platform's default), or with `--dir DIR`
into a directory for `espressif.rom.elfDirectory`; `--archive FILE` uses an
archive downloaded already.

### Headless use and the `espressif.rom` metadata

Headless, `espressif.rom.autoDownload` must be in the user settings
(`settings.json` in the user directory, or `binaryninja.Settings()` before
loading): it is global, and as a load option it is ignored.
`espressif.rom.elfDirectory` can be either.

To check what a view's ROM region holds, read its `espressif.rom` metadata
with one call, `bv.query_metadata("espressif.rom")`:

```python
{"status": "mapped", "chip": "esp32", "image": "esp32_rev0_rom.elf",
 "revision": 0, "source": "cache", "release": "20260528", "sha256": ...,
 "dataAlias": True}
```

- `status` is `mapped`, or for a region of zeros `zeros` (no image found,
  or `espressif.rom.loadImages` off), `downloading` (a download is running)
  or `failed` (this session's last download failed or timed out; the log
  says why); the image's fields come only with `mapped`.
- `source` is `elfDirectory`, `esp-idf` or `cache`, and `release` the
  esp-rom-elfs version of the latter two.
- `dataAlias` says whether the ROM's data bus alias (`espressif.rom.data`)
  is mapped too.

It is saved in databases, so it names no absolute path. A view without it
has no ROM region (the ESP8266, or a file that maps the ROM itself, such as
a chip's ROM ELF file opened directly). `espressif.rom.image`, the ROM ELF
file's name (`KeyError` with zeros), is kept too.

A database keeps the ROM region it was saved with; to map the image into
one saved with zeros, run the command on it and save:

```python
ctx = binaryninja.PluginCommandContext(bv)
binaryninja.PluginCommand.get_valid_list(ctx)["Espressif\\Download ROM Images"].execute(ctx)
# it runs on a thread of its own: wait until
# bv.query_metadata("espressif.rom")["status"] == "mapped", then
bv.update_analysis_and_wait()
bv.file.save_auto_snapshot()
```

### ROM functions

With real code in the ROM, every function the ROM ELF file's symbol table
names becomes a function and is analyzed (about 2,000 to 2,300 per chip),
so the ROM reads like any other code. The `espressif.rom.createFunctions`
setting turns that off: the ROM's other functions are then only named, and
analysis creates just the typed ROM functions and the ROM functions they
call (about 80 more on the ESP32, 450 to 600 on the RISC-V chips and the
ESP32-S3, whose ROM API is a jump table to the implementations), for a
smaller database and a faster analysis.

The typed ROM functions' names, prototypes and purity are made user data,
as the analysis of a body would change them (a jump table entry is named
`j_sub_...` after its target, `void *` parameters narrow to `uint8_t *`,
`__pure` is inferred), and the functions behind the jump table get the same
prototypes. They still clobber the caller-saved registers, the API's
contract, whatever their bodies do.

The ROM image and the analysis of its functions add to a database: the
ESP32 test ELF's is 2.2 MB without an image, 4.3 MB with one and
`espressif.rom.createFunctions` off, and 12 MB with it on (creating the
ROM's functions adds 4 to 8 MB on each chip, and a second or two of
analysis).

## The native library loader

Binary Ninja's extension manager only installs Python plugins, from the
source of the latest release, so it installs a small Python loader
(`__init__.py` and `binja_espressif_loader/`) that installs the prebuilt
native library from the matching GitHub release into the user plugin
folder.

- Prebuilt libraries exist for Linux x86_64, Windows x86_64 and macOS
  (universal). They are built for the core ABI of the Binary Ninja dev
  channel at release time (the binaryninja-api revision pinned in
  `Cargo.toml`). Each library's name carries its core ABI, and the loader
  picks the one the running core accepts. For any other platform, or a
  Binary Ninja whose core ABI range doesn't include the release's (such as
  stable, or a much newer dev build), it logs what is available and does
  nothing.
- In the UI it asks once whether to download the library, and again only
  when a new version needs a different library. Headless it downloads only
  with `espressif.loader.autoDownload` on.
- The library is downloaded over HTTPS from this repository's GitHub
  release (redirects are only followed to GitHub's release asset storage),
  and is installed only if its size and SHA-256 match
  `binja_espressif_loader/manifest.json`, which is part of the plugin's
  source as the extension manager installed it; nothing fetched from the
  release is trusted. It is written to a temporary file in the plugin
  folder, checked, and then renamed to `binja_espressif-native.so`
  (`.dll`, `.dylib`), so a new version replaces the old one and only one is
  ever installed.
- The loader does not load the library from Python: Binary Ninja loads it
  like any native plugin, after the plugins it depends on, on the next
  start.
- If the plugin folder already has a copy of the plugin, such as a
  `libbinja_espressif.so` you built (any `*binja_espressif*` library), the
  loader doesn't install a second one, which would register the same
  architectures twice, and logs why. If the native plugin is already loaded
  from elsewhere, it doesn't ask.
- **Plugins > Espressif > Remove Native Library** deletes the installed
  library; the loader then doesn't ask about that version again, and turns
  `espressif.loader.autoDownload` off if it was on.

## Limitations

- **ESP32-P4.** Its images are not loaded (the log says so), and its ELFs
  are not recognised as Espressif firmware. Its custom instructions (the
  PIE SIMD extension, in two incompatible versions for chip revisions
  before and from v3.0, and the hardware loops) are not decoded; Binary
  Ninja's `rv32gc` treats them as invalid instructions.
- **ESP32-C61, H21, H4 and S31.** Their images are not loaded either: the
  log names the chip ("ESP image for ESP32-C61 (chip id 20) ... is not
  supported by binja-espressif"), or gives the raw `chip_id` of an image
  for a chip it does not know, and the file opens as it would without the
  plugin.
- **Assembling** is not supported on the Xtensa architectures.
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
- **ESP8266 Wi-Fi and analog registers.** Its SVD file has no registers for
  the Wi-Fi MAC and baseband (WDEV, 0x3FF20000 on, apart from the RNG
  register at 0x3FF20E44), the SAR ADC or the analog blocks behind the
  internal I2C master; those addresses stay unnamed and unmapped.
- **Zcb/Zcmp on the ESP32-C5.** ESP-IDF only emits them with
  `CONFIG_COMPILER_ENABLE_RISCV_ZCMP` (off by default). `rv32gc` decodes
  `cm.push`/`cm.pop` as `fsd` and does not decode the Zcb instructions, so
  firmware built with that option analyzes incorrectly.
- **"null arch" errors when a RISC-V database is reopened.** Binary
  Ninja logs "Unable to restore arch id mappings for null arch" and
  "Failed to translate saved id (1, 3) due to null arch!", usually twice,
  when it reopens a database whose view has a global pointer value: on
  the RISC-V chips, ESP-IDF firmware and some ROM ELF files. They are
  harmless: Binary Ninja restores the view's global pointer values without
  the architecture that would translate their register (`gp`, register 3
  of `rv32gc`), and keeps the register as saved, so `gp` comes back as it
  was. It does the same without the plugin, for any architecture.
- **Stable Binary Ninja.** Releases are built for the dev channel only for
  now; build from source against the stable API (see
  [development.md](development.md#building-for-stable-binary-ninja)).
