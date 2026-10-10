//! Binary Ninja plugin for Espressif's chips: the architectures of the Xtensa
//! cores of the ESP8266, ESP32, ESP32-S2 and ESP32-S3 (`xtensa`), and the
//! loading of Espressif firmware images, ELF files, ROM symbols and SVD files
//! (`espressif`).

pub mod espressif;
pub mod xtensa;

use binaryninja::add_optional_plugin_dependency;

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginInit() -> bool {
    binaryninja::tracing_init!("Espressif");

    xtensa::register();
    espressif::register();
    true
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginDependencies() {
    add_optional_plugin_dependency("view_elf");
    // The SVD file selection step is inserted before the SVD loader's step.
    add_optional_plugin_dependency("svd_ninja");
}
