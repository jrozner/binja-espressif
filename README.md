# binja-espressif

A Binary Ninja plugin for Espressif's chips. It adds architectures for the
Xtensa cores of the **ESP8266**, **ESP32**, **ESP32-S2** and **ESP32-S3**,
and loads firmware for those and for the RISC-V **ESP32-C2**, **C3**,
**C5**, **C6** and **H2** (analyzed with Binary Ninja's own `rv32gc`
architecture).

- **Xtensa architectures** for each core, decoding exactly the instructions
  it implements (MAC16, FPU, the S3's PIE vector extension and more),
  disassembling like GNU objdump and lifting the whole ISA, with the
  `windowed` and `call0` calling conventions and zero-overhead loops.
- **Firmware images, flash dumps and ELF files** open on the right chip,
  mapped into its memory map.
- **ROM function names and types**, so calls into the mask ROM decompile
  with real names and arguments (`ets_printf`, `memset`, `__udivdi3`).
- **ROM images**, downloaded with your consent, so the ROM's own code can be
  read and decompiled.
- **Peripheral registers** named and typed from the chips' SVD files
  (`UART0.INT_CLR.RXFIFO_FULL_INT_CLR = 9`).

## Install

Install **Espressif** from Binary Ninja's extension manager. For now it
needs a **dev** build of Binary Ninja; stable is not supported yet.

The plugin is a native library, which the extension manager can't install
itself, so what it installs is a small Python loader that fetches the
prebuilt library (Linux x86_64, Windows x86_64, macOS) from this
repository's GitHub release:

1. After installing, the loader asks once whether to download the native
   library. If you say no, you can run **Plugins > Espressif > Install or
   Update Native Library** later.
2. Restart Binary Ninja to load it.

The download is checked against SHA-256 hashes shipped with the plugin. If
there is no library for your platform or Binary Ninja version, the loader
logs what's available and installs nothing; [build from
source](#building-from-source) instead. If you've already put a library
you built yourself in the plugin folder, the loader leaves it alone.

**Headless**, the loader never asks: it downloads only if the
`espressif.loader.autoDownload` setting is on (in Settings, or
`{"espressif.loader.autoDownload": true}` in `settings.json` in your user
folder).

**Uninstalling:** removing the plugin in the extension manager doesn't
remove the native library. Run **Plugins > Espressif > Remove Native
Library** first (or delete `binja_espressif-native.so`, `.dll` or `.dylib`
from your [user plugin folder](https://docs.binary.ninja/guide/index.html#user-folder)),
then restart.

## Usage

Open a file and the plugin picks the chip:

- **Firmware images** (ESP-IDF app and bootloader `.bin` files, ESP8266 v1
  and v2 images) open in the `ESP` view, with the segments, RAM, RTC memory
  and peripherals mapped, the ROM's functions named and typed, and the app
  description typed.
- **ELF files** (`EM_XTENSA`, or 32-bit soft-float RISC-V that fits a
  chip's memory map) open on the chip's platform, with ESP-IDF's layout
  fixed up. If the plugin guesses the wrong chip, choose it in *Open with
  Options > Platform* (`esp32`, `esp32s3`, `esp32c3`, ...). Relocatable
  objects and static library members (such as ESP-IDF's binary Wi-Fi
  libraries) load with their relocations applied.
- **Flash dumps** with a partition table open in Binary Ninja's container
  browser, listing the bootloader, the partition table and each partition
  by label; pick the one to analyze. Headless, `bn.load` opens the factory
  app; choose another with the `files.espressif.partition` load option
  (`"ota_0"`, `"bootloader"`, or `"*"` for the whole dump).
- **Espressif's ROM ELF files** (from
  [esp-rom-elfs](https://github.com/espressif/esp-rom-elfs)) open on their
  chip with the ROM functions typed.

### ROM images

Firmware calls into the chip's mask ROM all the time. The plugin always
names and types the ROM's functions, and with a ROM image it also maps the
ROM's real code and data, so you can follow calls into it.

The images aren't bundled. The first time a file could use one, Binary
Ninja asks whether to download them from esp-rom-elfs (about 5 MB, once).
You can also run **Plugins > Espressif > Download ROM Images** at any time;
it maps the image into the current file. If ESP-IDF's tools are installed
(`~/.espressif/tools/esp-rom-elfs`), their copy is used without downloading
anything.

### Settings

Everything is under *Espressif* in Settings. The ones you're most likely to
want:

- `espressif.rom.autoDownload`: download ROM images without asking (the
  only way they download headless).
- `espressif.rom.elfDirectory`: a directory of ROM ELF files to use.
- `espressif.rom.createFunctions`: turn off to only name the ROM's
  functions rather than analyze them all (a smaller, faster database).
- `espressif.svd.autoLoad`: turn off to skip the bundled SVD files.
- `files.espressif.partition`: the flash dump partition to open headless.

## Limitations

- The **ESP32-P4**, **C61**, **H21**, **H4** and **S31** aren't supported
  yet: their images aren't loaded (the log says so), and P4 ELF files open
  as generic RISC-V.
- Assembling isn't supported on the Xtensa architectures.
- **ESP8266** ROM functions are named but not typed, and its SVD file lacks
  the Wi-Fi, ADC and analog registers.
- **ESP32-C5** firmware built with `CONFIG_COMPILER_ENABLE_RISCV_ZCMP` (off
  by default) analyzes incorrectly.
- Peripheral names need Binary Ninja's SVD loader plugin (`svd_ninja`).

See [docs/reference.md](docs/reference.md#limitations) for the full list
and for how each feature behaves in detail.

## Building from source

You need a recent stable Rust, libclang (`libclang-dev` on Debian and
Ubuntu) and a Binary Ninja whose version matches the binaryninja-api
revision in `Cargo.toml` (a dev build on `main`). The build finds Binary Ninja through
`~/.binaryninja/lastrun` or `~/binaryninja`, or set `BINARYNINJADIR`.

```sh
cargo build --release
cp target/release/libbinja_espressif.so ~/.binaryninja/plugins/   # .dylib on macOS, .dll on Windows
```

On Windows, run `git config --global core.longpaths true` first. To build
without Binary Ninja installed (for example to run the unit tests), add
`--no-default-features`: that links a stub core instead (and needs CMake
and Ninja). See
[docs/development.md](docs/development.md) for building against stable,
testing and the repository layout.

## License

MIT, see [LICENSE](LICENSE). The plugin also includes:

- ROM symbol tables derived from ESP-IDF's ROM linker scripts (Apache-2.0,
  Espressif) and the ESP8266 RTOS and NONOS SDKs' (Apache-2.0 and the
  Espressif MIT License);
- ROM type libraries derived from ESP-IDF v6.1's headers (Apache-2.0,
  Espressif) and newlib's headers (BSD-style, see newlib's `COPYING.NEWLIB`);
- Espressif's SVD files (Apache-2.0), and an ESP8266 SVD file generated from
  [esp-rs/esp-pacs](https://github.com/esp-rs/esp-pacs) (MIT or Apache-2.0).
  See [view/espressif/svd/README.md](view/espressif/svd/README.md) for
  sources and licenses.

The ROM images are not part of the plugin. They're downloaded, with your
consent, from [esp-rom-elfs](https://github.com/espressif/esp-rom-elfs),
whose ELF files are licensed under Apache-2.0; the ROM code is closed
source, Copyright Espressif Systems, and includes the third-party libraries
listed in [ESP-IDF's copyrights page](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/COPYRIGHT.html#rom-source-code-copyrights).

## Contributing

See [docs/development.md](docs/development.md) for building, testing and
regenerating the bundled data, [docs/reference.md](docs/reference.md) for
how the plugin works in detail, and [docs/releasing.md](docs/releasing.md)
for cutting and testing a release.
