//! The Binary Ninja plugin for Espressif's chips that the release ships: one
//! library holding
//!
//! * the Xtensa architectures of the ESP8266, ESP32, ESP32-S2 and ESP32-S3
//!   (the `arch_xtensa` crate, `arch/xtensa`),
//! * the soft-float `ilp32` calling convention for Binary Ninja's RISC-V
//!   architecture, used by the ESP32-C2, C3, C6, H2 and C5 (`riscv`),
//! * the loading of Espressif firmware images, ELF files, ROM symbols and
//!   SVD files (`espressif`).
//!
//! Each is registered as a plugin of its own would be. They are independent:
//! `espressif` uses no type of the others, but looks the architectures and
//! calling conventions up by name, and the architectures know nothing of
//! Espressif's chips.

pub mod espressif;
pub mod riscv;

use binaryninja::add_optional_plugin_dependency;
use binaryninja::tracing::{BinaryNinjaLayer, init_with_layer};

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginInit() -> bool {
    // Each crate logs under the name it has as a plugin of its own.
    let layer = BinaryNinjaLayer::default()
        .with_target_mapping("arch_xtensa", "Xtensa")
        .with_target_mapping(env!("CARGO_CRATE_NAME"), "Espressif");
    init_with_layer(layer);

    arch_xtensa::register();
    riscv::register();
    espressif::register();
    true
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginDependencies() {
    arch_xtensa::plugin_dependencies();
    // The RISC-V chips are analyzed with Binary Ninja's rv32gc architecture.
    add_optional_plugin_dependency("arch_riscv");
    // The SVD file selection step is inserted before the SVD loader's step.
    add_optional_plugin_dependency("svd_ninja");
}
