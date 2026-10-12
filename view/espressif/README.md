# view_espressif

The Binary Ninja plugin for Espressif's chips: the ESP8266, ESP32, ESP32-S2,
ESP32-S3 (Xtensa) and ESP32-C2, C3, C6, H2, C5 (RISC-V). It provides:

- A platform per chip (`esp8266`, `esp32`, ... `esp32c5`, the ESP-IDF target
  names, under the OS `espressif`) on the chip's architecture.
- The `ESP` view for ESP-IDF and ESP8266 firmware images and flash dumps,
  and the `EspFlash` container transform that splits a flash dump into its
  bootloader, partition table and partitions.
- Chip recognition and fix-ups for Xtensa and RISC-V ELF files of these
  chips, including Espressif's own ROM ELF files.
- The mask ROM's symbols (`data/rom_symbols/`, from the ROM linker scripts),
  types (`data/rom_types/`, type libraries generated from ESP-IDF's headers)
  and, on request, Espressif's ROM images (downloaded from esp-rom-elfs and
  verified against a pinned SHA-256).
- The chips' SVD files (`svd/`), selected for the chip and applied by
  Binary Ninja's SVD loader: Espressif's, and for the ESP8266 one generated
  with svdtools from esp-rs/esp-pacs' (see svd/README.md). That one covers
  DPORT, EFUSE, the RNG register, UART0/1, SPI0/1, GPIO, IO_MUX, the timers,
  RTC, WDT, SLC, I2S and the analog I2C master's register, but not the
  Wi-Fi MAC and baseband (WDEV, 0x3FF20000 on), the SAR ADC or the analog
  blocks.

It shares no Rust type with the architectures. It looks them up by name:
the `arch_xtensa` plugin's Xtensa architectures, and Binary Ninja's
`rv32gc` with the `ilp32` convention from the `arch_riscv_ilp32` plugin.
The `xtensa` decoder crate finds `entry` instructions when the view scans
code for functions.

See the [repository README](../../README.md) for how to use it and what it
does and does not do.

## Building

This crate is built two ways:

- On its own, as Binary Ninja's own Rust plugins are: `cargo build --release
  -p view_espressif` builds `libview_espressif.so` (`.dylib`,
  `view_espressif.dll`), which exports `CorePluginInit` and needs the
  `arch_xtensa` and `arch_riscv_ilp32` plugins next to it. `CMakeLists.txt`
  builds it with `bn_add_rust_crate` from the binaryninja-api tree.
- Bundled into the plugin the release ships (`binja-espressif`, the repository
  root), which links it as a Rust library with the `bundled` feature (no
  entry points of its own) and calls `register()` after registering the
  architectures.

`build.rs` compresses the SVD files into the library. Without the default
`core` feature, the crate links a stub core library that
`binaryninjacore-sys` builds, so it builds where Binary Ninja is not
installed.

## Testing

```sh
cargo test -p view_espressif --no-default-features --lib
```

The end-to-end tests (`tests/test_headless.py` in the repository root) run
the bundled plugin.
