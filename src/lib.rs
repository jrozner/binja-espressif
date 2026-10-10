//! Binary Ninja architecture plugin for the Xtensa cores of Espressif's
//! ESP8266, ESP32, ESP32-S2 and ESP32-S3.

pub mod espressif;
pub mod xtensa;

use binaryninja::add_optional_plugin_dependency;

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginInit() -> bool {
    binaryninja::tracing_init!("Xtensa");

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
