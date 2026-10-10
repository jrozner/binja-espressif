# binja-xtensa

A Binary Ninja architecture plugin for the Xtensa cores in Espressif's
**ESP8266**, **ESP32**, **ESP32-S2** and **ESP32-S3**, written in Rust against
the `dev` branch of the [binaryninja-api](https://github.com/Vector35/binaryninja-api).
Decoding is done by the [`xtensa`](https://github.com/jrozner/xtensa)
disassembler crate, which is verified against GNU objdump.

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
  code on the others). Functions that do not begin with `entry` on the
  windowed cores are automatically switched to `call0`, unless you have given
  the function a type or calling convention yourself (for example a windowed
  stub with no `entry`, typed `__convention("windowed")`). On `rv32gc`,
  `ilp32`, the soft-float ABI ESP-IDF builds the RISC-V chips for (the
  architecture's own `default` passes floating point values in `fa0`–`fa7`),
  is the RISC-V chip platforms' default.
- **Zero-overhead loops** (`loop`/`loopnez`/`loopgtz`) recovered as real loops.
- An **`ESP` binary view** for raw firmware:
  - ESP32/ESP32-S2/ESP32-S3 and ESP32-C2/C3/C6/H2/C5 application and
    bootloader images from any ESP-IDF version (the chip is taken from the
    extended header's `chip_id`; images from before that field existed are
    recognised as ESP32);
  - ESP8266 v1 images (NONOS/RTOS SDK, including the RTOS SDK v3 layout with an
    extended header) and v2 images (`0xEA`, NONOS OTA `user1.bin`, Arduino);
  - flash dumps, starting at flash offset 0 or 0x1000 (as `esptool merge_bin`
    and most firmware distributions produce for the ESP32/ESP32-S2). Only one
    partition is loaded, by default the factory (or first OTA) app. The
    `loader.esp.partition` load setting (*Open with Options*, or
    `bn.load(path, options={"loader.esp.partition": "ota_1"})`) picks another
    app partition by label, or `bootloader` for the second stage bootloader.
    ESP8266 NONOS dumps (no partition table) are mapped through the
    0x40200000 flash cache window so `irom0` code is present.

  The view maps every segment into the chip's memory map with appropriate
  permissions and section semantics, adds unbacked segments for `.bss`,
  RTC memory and the peripheral register blocks, names the mask ROM's
  functions and data (from the ESP-IDF and ESP8266 SDK linker scripts) and
  the ESP8266's peripheral blocks, types the ESP-IDF `esp_app_desc_t`, and,
  on the Xtensa chips, seeds functions from every aligned `entry`
  instruction so that callbacks and tasks reached only through pointers are
  found. On the RISC-V chips it finds ESP-IDF's interrupt vector table
  (`_vector_table`, or the CLIC's `_mtvt_table` on the ESP32-C5) and names
  and seeds its handlers, and reads the value `call_start_cpu0` loads into
  `gp` (`la gp, __global_pointer$`) so that `gp`-relative accesses to small
  data resolve from the start. The value is set as the view's global
  pointer value (*user* value, since Binary Ninja only allows setting those);
  one you set yourself, or saved in a database, is left alone. Bootloader
  images do not use `gp`. On the ESP32-C6, H2 and C5, where SRAM is one
  region seen by both buses and flash is mapped through one window shared
  by code and read-only data, the SRAM segments are mapped writable and
  executable and the flash window read-only, as code.
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
  revision only, as an ELF does not say which revision it needs); and the
  RISC-V global pointer is taken from `__global_pointer$`.

Patching and assembling are not supported.

Not supported for now:

- **ESP32-P4.** Its images are not loaded, and its ELFs are not recognised
  as Espressif firmware. Its custom instructions (the PIE SIMD extension,
  in two incompatible versions for chip revisions before and from v3.0, and
  the hardware loops) are not decoded; Binary Ninja's `rv32gc` treats them
  as invalid instructions.
- **Zcb/Zcmp on the ESP32-C5.** ESP-IDF only emits them with
  `CONFIG_COMPILER_ENABLE_RISCV_ZCMP` (off by default). `rv32gc` decodes
  `cm.push`/`cm.pop` as `fsd` and does not decode the Zcb instructions, so
  firmware built with that option analyzes incorrectly.

## Building and installing

Requires a Rust toolchain recent enough for the binaryninja-api `dev` branch
(see its `rust-version`; 1.98.1 at the time of writing) and a Binary Ninja
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
`build.rs` compresses the files of the supported chips into the plugin.

To move to a newer API revision, `cargo update -p binaryninja` and rebuild
against a matching Binary Ninja build.

## Testing

```sh
cargo test --lib                          # image formats, partitions, loops, intrinsics
cargo test --release --test lift_sweep    # lifts ~1.5M encodings through the real core
cargo build --release && tests/test_headless.py
```

The last two need a headless-capable Binary Ninja license; `lift_sweep`
skips itself when the core cannot be initialized. `tests/test_headless.py`
runs Binary Ninja with a throwaway user directory containing only this plugin
and checks ELF loading, chip detection, calling conventions, call-site
arguments, loop recovery, lifting semantics, and the ESP view (including
partition selection and malformed images) on images and flash dumps
synthesized from `tests/data/*.elf`. Those ELFs are built from
`tests/data/corpus.c` by `scripts/build_test_elfs.sh` with Espressif's GCC.

## License

MIT, see [LICENSE](LICENSE). The Espressif SVD files embedded in the plugin
(from the `svd` submodule) are licensed under Apache-2.0 by Espressif.
