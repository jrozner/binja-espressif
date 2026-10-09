# binja-xtensa

A Binary Ninja architecture plugin for the Xtensa cores in Espressif's
**ESP8266**, **ESP32**, **ESP32-S2** and **ESP32-S3**, written in Rust against
the `dev` branch of the [binaryninja-api](https://github.com/Vector35/binaryninja-api).
Decoding is done by the verified [`xtensa`](../xtensa) disassembler crate.

It provides:

- **Architectures** `xtensa-esp8266`, `xtensa-esp32`, `xtensa-esp32s2` and
  `xtensa-esp32s3`, each decoding exactly the instructions its core
  implements (including MAC16, booleans, FPU, the ESP32 DFP assist,
  the S2 dedicated-GPIO TIE and the S3 PIE vector extension), and a
  standalone platform of the same name.
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
  code on the others). Functions that do not begin with `entry` on the
  windowed cores are automatically switched to `call0`.
- **Zero-overhead loops** (`loop`/`loopnez`/`loopgtz`) recovered as real loops.
- An **`ESP` binary view** for raw firmware:
  - ESP32/ESP32-S2/ESP32-S3 application and bootloader images from any
    ESP-IDF version (the chip is taken from the extended header's `chip_id`;
    images from before that field existed are recognised as ESP32);
  - ESP8266 v1 images (NONOS/RTOS SDK, including the RTOS SDK v3 layout with an
    extended header) and v2 images (`0xEA`, NONOS OTA `user1.bin`, Arduino);
  - flash dumps, starting at flash offset 0 or 0x1000 (as `esptool merge_bin`
    and most firmware distributions produce for the ESP32/ESP32-S2): the
    only one partition is loaded, by default the factory (or first OTA) app.
    The `loader.esp.partition` load setting (*Open with Options*, or
    `bn.load(path, options={"loader.esp.partition": "ota_1"})`) picks another
    app partition by label, or `bootloader` for the second stage bootloader; ESP8266 NONOS dumps (no partition table) are mapped
    through the 0x40200000 flash cache window so `irom0` code is present.

  The view maps every segment into the chip's memory map with appropriate
  permissions and section semantics, adds unbacked segments for `.bss`,
  RTC memory and the peripheral register blocks, names the mask ROM's
  functions and data (from the ESP-IDF and ESP8266 SDK linker scripts) and
  the ESP8266's peripheral blocks, types the ESP-IDF `esp_app_desc_t`, and seeds
  functions from every aligned `entry` instruction so that callbacks and
  tasks reached only through pointers are found.
- **Peripheral registers from SVD**: Espressif's SVD files for the ESP32,
  ESP32-S2 and ESP32-S3 (https://github.com/espressif/svd, Apache-2.0) are
  bundled (the `svd` git submodule) and applied automatically to ESP firmware images by Binary
  Ninja's SVD loader, naming and typing every peripheral register
  (`UART0.INT_CLR.RXFIFO_FULL_INT_CLR = 9`). An SVD file chosen in *Open with
  Options* (*SVD File*, `analysis.svd.autoLoadFile`) takes precedence, and
  `xtensa.svd.autoLoad` turns this off. The files are unpacked once to
  `<user directory>/xtensa/svd/`. The ESP8266 has no official SVD, so the
  view names its peripheral blocks itself.
- **ELF support**: `EM_XTENSA` files open with the ELF view; the chip is
  recognised from the program headers (and, for ambiguous layouts, from
  whether the entry point uses the windowed ABI).

Patching and assembling are not supported.

See [LIMITATIONS.md](LIMITATIONS.md) for what Binary Ninja cannot express,
bugs found along the way, and the plugin's own approximations.

## Building and installing

Requires a Rust toolchain recent enough for the binaryninja-api `dev` branch
(see its `rust-version`; 1.99 at the time of writing) and a Binary Ninja
installation whose core matches the `dev` API revision pinned in `Cargo.lock`
(`binaryninjacore-sys` finds the installation through
`~/.binaryninja/lastrun` or `~/binaryninja`; set `BINARYNINJADIR` to
override).

```sh
git submodule update --init      # Espressif's SVD files (or clone with --recursive)
cargo build --release
cp target/release/libbinja_xtensa.so ~/.binaryninja/plugins/   # .dylib / .dll elsewhere
```

To pick up newer SVD files, `git submodule update --remote svd` and rebuild;
`build.rs` compresses the ESP32, ESP32-S2 and ESP32-S3 files into the
plugin.

To move to a newer API revision, `cargo update -p binaryninja` and rebuild
against a matching Binary Ninja build.

## Testing

```sh
cargo test --lib                          # image parsing, intrinsic classification
cargo test --release --test lift_sweep    # lifts ~1.5M encodings through the real core
cargo build --release && tests/test_headless.py
```

The last two need a headless-capable Binary Ninja license; `lift_sweep`
skips itself when the core cannot be initialized. `tests/test_headless.py`
runs Binary Ninja with a throwaway user directory containing only this plugin
and checks ELF loading, chip detection, calling conventions, call-site
arguments, loop recovery, and the ESP view on images and flash dumps
synthesized from `tests/data/*.elf`. Those ELFs are built from
`tests/data/corpus.c` by `scripts/build_test_elfs.sh` with Espressif's GCC.

The plugin has also been exercised on MicroPython release firmware for all
four chips, including 2017 (ESP8266, NONOS SDK) and 2019 (ESP32, ESP-IDF 3.3)
builds.

## Layout

| file | |
|---|---|
| `src/lib.rs` | plugin entry point, registration, ELF platform recognizer |
| `src/arch.rs` | the `Architecture` implementation |
| `src/registers.rs`, `src/flags.rs` | register and flag models |
| `src/lift.rs` | LLIL lifting |
| `src/loops.rs` | zero-overhead loop tracking |
| `src/intrinsics.rs` | intrinsic definitions and operand classification |
| `src/cc.rs` | calling conventions and the windowed-function recognizer |
| `src/tokens.rs` | instruction text |
| `src/esp.rs` | image formats, partition tables, memory maps, chip detection |
| `src/view.rs` | the `ESP` binary view |
| `src/exemplars.rs` | generated by `scripts/gen_exemplars.py` |
| `src/rom_symbols.rs` | generated by `scripts/gen_rom_symbols.py` |
| `src/svd.rs` | automatic loading of the bundled SVD files |
| `svd/` | git submodule of [espressif/svd](https://github.com/espressif/svd); `build.rs` compresses the files the plugin embeds |
