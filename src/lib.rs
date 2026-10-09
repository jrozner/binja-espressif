//! Binary Ninja architecture plugin for the Xtensa cores of Espressif's
//! ESP8266, ESP32, ESP32-S2 and ESP32-S3.

pub mod arch;
pub mod cc;
pub mod esp;
pub mod exemplars;
pub mod flags;
pub mod intrinsics;
pub mod lift;
pub mod loops;
pub mod registers;
pub mod rom_symbols;
pub mod svd;
pub mod tokens;
pub mod view;

use binaryninja::architecture::{ArchitectureExt, register_architecture};
use binaryninja::binary_view::{BinaryView, BinaryViewType, register_binary_view_type};
use binaryninja::platform::Platform;
use binaryninja::rc::Ref;
use binaryninja::{Endianness, add_optional_plugin_dependency};

use crate::arch::{Chip, Xtensa};

/// `EM_XTENSA`
const ELF_MACHINE_XTENSA: u32 = 94;

/// Picks the chip of an Xtensa ELF file from its program headers.
fn recognize_elf(view: &BinaryView) -> Option<Ref<Platform>> {
    let header = view.read_vec(0, 0x34);
    if header.len() < 0x34 || header[4] != 1 {
        return None; // not ELF32
    }
    let u16_at = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let entry = u32_at(&header, 0x18);
    let (phoff, phentsize, phnum) = (
        u32_at(&header, 0x1c) as u64,
        u16_at(&header, 0x2a),
        u16_at(&header, 0x2c),
    );
    let mut addrs = Vec::new();
    let mut entry_code = None;
    for i in 0..phnum.min(64) {
        let ph = view.read_vec(phoff + (i * phentsize) as u64, 32);
        if ph.len() < 32 || u32_at(&ph, 0) != 1 {
            continue; // PT_LOAD only
        }
        let (offset, vaddr, filesz) = (u32_at(&ph, 4), u32_at(&ph, 8), u32_at(&ph, 16));
        addrs.push(vaddr);
        if vaddr <= entry && entry < vaddr.wrapping_add(filesz) {
            entry_code = Some(view.read_vec((offset + entry - vaddr) as u64, 3));
        }
    }
    let chip = esp::guess_chip(&addrs, entry_code.as_deref());
    tracing::info!("Xtensa ELF looks like {} code", chip.display_name());
    Platform::by_name(chip.arch_name())
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginInit() -> bool {
    binaryninja::tracing_init!("Xtensa");

    let mut esp32 = None;
    for chip in Chip::ALL {
        let arch = register_architecture(chip.arch_name(), move |custom_handle, handle| {
            Xtensa::new(chip, handle, custom_handle)
        });
        let conventions = cc::register(arch);
        if chip.windowed() {
            arch.register_function_recognizer(cc::WindowRecognizer {
                call0: conventions.call0,
            });
        }
        if chip == Chip::Esp32 {
            esp32 = Some(arch);
        }
    }

    svd::register_settings();
    svd::register_activity();
    register_binary_view_type(view::EspViewType);

    if let Some(elf) = BinaryViewType::by_name("ELF") {
        if let Some(esp32) = esp32 {
            elf.register_arch(ELF_MACHINE_XTENSA, Endianness::LittleEndian, esp32);
        }
        elf.register_platform_recognizer(
            ELF_MACHINE_XTENSA,
            Endianness::LittleEndian,
            |view, _| recognize_elf(view),
        );
    }
    true
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginDependencies() {
    add_optional_plugin_dependency("view_elf");
    // The SVD file selection step is inserted before the SVD loader's step.
    add_optional_plugin_dependency("svd_ninja");
}
