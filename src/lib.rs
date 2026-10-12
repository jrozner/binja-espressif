//! The Binary Ninja plugin for Espressif's chips that the release ships: one
//! library holding three plugins, each of which also builds on its own:
//!
//! * `arch_xtensa` (`arch/xtensa`): the Xtensa architectures of the ESP8266,
//!   ESP32, ESP32-S2 and ESP32-S3;
//! * `arch_riscv_ilp32` (`arch/riscv_ilp32`): the soft-float `ilp32` calling
//!   convention for Binary Ninja's RISC-V architecture, used by the
//!   ESP32-C2, C3, C6, H2 and C5;
//! * `view_espressif` (`view/espressif`): the loading of Espressif firmware
//!   images, flash dumps, ELF files, ROM symbols, types and images, and SVD
//!   files, and the chip platforms.
//!
//! They are linked in with their `bundled` feature, which leaves their entry
//! points out, and registered here in the order Binary Ninja would load them
//! in as separate plugins: the view looks the architectures and the calling
//! convention up by name.

use binaryninja::tracing::{BinaryNinjaLayer, init_with_layer};

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginInit() -> bool {
    // Each crate logs under the name it has as a plugin of its own.
    let layer = BinaryNinjaLayer::default()
        .with_target_mapping("arch_xtensa", "Xtensa")
        .with_target_mapping("arch_riscv_ilp32", "RISC-V")
        .with_target_mapping("view_espressif", "Espressif");
    init_with_layer(layer);

    arch_xtensa::register();
    arch_riscv_ilp32::register();
    view_espressif::register();
    true
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginDependencies() {
    // The plugins' own dependencies on each other are satisfied here; the
    // others (view_elf, arch_riscv, svd_ninja) are declared once each would
    // be, which the core accepts.
    arch_xtensa::plugin_dependencies();
    arch_riscv_ilp32::plugin_dependencies();
    view_espressif::plugin_dependencies();
}
