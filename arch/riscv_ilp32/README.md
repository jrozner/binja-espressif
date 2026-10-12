# arch_riscv_ilp32

A Binary Ninja plugin that adds the `ilp32` calling convention to Binary
Ninja's `rv32gc` architecture (the `arch_riscv` plugin). `ilp32` is the
soft-float RISC-V integer ABI that ESP-IDF builds Espressif's RISC-V chips
for. They have no FPU, while `rv32gc`'s `default` convention passes floating
point values in `fa0`-`fa7`.

Under `ilp32`, every argument goes in the integer registers `a0`-`a7`, one
register per 4 bytes (a value of 5 to 8 bytes takes the next two, with no
even-register alignment), and the rest on the stack. Results are in
`a0`/`a1`, `s0`-`s11` are callee-saved, and `gp` is the global pointer.

It knows nothing about Espressif's chips. The Espressif view
(`view_espressif`) makes it the default convention of the RISC-V chip
platforms. It belongs in `arch_riscv` itself and lives here until it is
proposed there.

## Building

On its own: `cargo build --release -p arch_riscv_ilp32` builds
`libarch_riscv_ilp32.so` (`.dylib`, `arch_riscv_ilp32.dll`), a plugin that
needs `arch_riscv`. `CMakeLists.txt` builds it with `bn_add_rust_crate` from
the binaryninja-api tree. The release's plugin (`binja-espressif`, the
repository root) links it with the `bundled` feature and calls `register()`.

```sh
cargo test -p arch_riscv_ilp32 --no-default-features --lib
```
