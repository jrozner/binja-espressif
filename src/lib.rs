//! Binary Ninja plugin for Espressif's chips: the architectures of the Xtensa
//! cores of the ESP8266, ESP32, ESP32-S2 and ESP32-S3 (`xtensa`), the
//! soft-float `ilp32` calling convention for Binary Ninja's RISC-V
//! architecture, used by the ESP32-C2, C3, C6, H2 and C5 (`riscv`), and the
//! loading of Espressif firmware images, ELF files, ROM symbols and SVD files
//! (`espressif`).
//!
//! The three are independent: `espressif` uses no type of the other two,
//! but looks the architectures and calling conventions up by name, and the
//! architectures know nothing of Espressif's chips.

pub mod espressif;
pub mod riscv;
pub mod xtensa;

use binaryninja::add_optional_plugin_dependency;

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginInit() -> bool {
    binaryninja::tracing_init!("Espressif");

    xtensa::register();
    riscv::register();
    espressif::register();
    true
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginDependencies() {
    add_optional_plugin_dependency("view_elf");
    // The RISC-V chips are analyzed with Binary Ninja's rv32gc architecture.
    add_optional_plugin_dependency("arch_riscv");
    // The SVD file selection step is inserted before the SVD loader's step.
    add_optional_plugin_dependency("svd_ninja");
}
