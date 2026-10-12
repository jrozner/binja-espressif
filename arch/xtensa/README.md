# arch_xtensa

The Binary Ninja architecture plugin for the Xtensa cores of Espressif's
chips: the ESP8266 (LX106), ESP32 (LX6), ESP32-S2 and ESP32-S3 (LX7). It
knows nothing about Espressif's firmware formats, memory maps or ROMs; the
Espressif view (`view_espressif`) handles those and looks the architectures
up by name.

It provides:

- An architecture per core, `xtensa-esp8266`, `xtensa-esp32`,
  `xtensa-esp32s2` and `xtensa-esp32s3`, each decoding exactly the
  instructions its core implements (with the `xtensa` decoder crate in this
  repository) and disassembling in GNU objdump syntax.
- Lifting of the whole ISA to LLIL: native semantics for the integer,
  load/store, branch, call, shift/SAR, zero-overhead loop, boolean, floating
  point and conditional-move instructions, and intrinsics named after their
  mnemonics, and for special and user register accesses, for the rest.
- The `windowed` (`call8`) and `call0` calling conventions, with an analysis
  step that gives the functions without `entry` on the windowed cores the
  `call0` convention, and a function recognizer that fixes up what windowed
  functions clobber and types `callx0` sites. Functions whose type was
  imported from a type library (such as Espressif's ROM functions) keep the
  convention and clobbers their type says.
- Zero-overhead loops, patching (`convert_to_nop`, `invert_branch`,
  `always_branch`, skip-and-return), an ELF relocation handler, and the
  ESP32's architecture for `EM_XTENSA` ELF files.

See [docs/reference.md](../../docs/reference.md) for what the lifting does
and does not model.

## Building

This crate is built two ways:

- On its own, as Binary Ninja's own Rust plugins are: `cargo build --release
  -p arch_xtensa` builds `libarch_xtensa.so` (`.dylib`, `arch_xtensa.dll`),
  which exports `CorePluginInit`. `CMakeLists.txt` builds it with
  `bn_add_rust_crate` from the binaryninja-api tree.
- Bundled into the plugin the release ships (`binja-espressif`, the repository
  root), which links it as a Rust library with the `bundled` feature (no
  entry points of its own) and calls `register()`.

Without the default `core` feature, it links a stub core library that
`binaryninjacore-sys` builds, so it builds where Binary Ninja is not
installed.

## Testing

```sh
cargo test -p arch_xtensa --no-default-features --lib
# Lifts ~1.5M encodings through the real core (needs a headless-capable
# Binary Ninja license).
cargo test --release -p arch_xtensa --features liftcheck --lib liftcheck::lift_sweep
# Prints the decoded operands of every opcode.
cargo test -p arch_xtensa --no-default-features --features liftcheck --lib liftcheck::operands -- --ignored --nocapture
```

The end-to-end tests (`tests/test_headless.py` in the repository root) run
the bundled plugin.
