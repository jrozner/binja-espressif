//! Espressif's chips: their firmware image formats, memory maps, ROM
//! symbols and peripheral registers, and the binary view that loads their
//! firmware. Each chip's code is analyzed with the architecture of its CPU
//! core, looked up by name.

pub mod image;
pub mod rom_symbols;
pub mod svd;
pub mod view;

use binaryninja::Endianness;
use binaryninja::architecture::CoreArchitecture;
use binaryninja::binary_view::{BinaryView, BinaryViewType, register_binary_view_type};
use binaryninja::platform::Platform;
use binaryninja::rc::Ref;

use crate::xtensa::{self, Core};

/// A supported chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chip {
    Esp8266,
    Esp32,
    Esp32S2,
    Esp32S3,
}

impl Chip {
    pub const ALL: [Chip; 4] = [Chip::Esp8266, Chip::Esp32, Chip::Esp32S2, Chip::Esp32S3];

    /// The chip's CPU core.
    pub fn core(self) -> Core {
        match self {
            Chip::Esp8266 => Core::Esp8266,
            Chip::Esp32 => Core::Esp32,
            Chip::Esp32S2 => Core::Esp32S2,
            Chip::Esp32S3 => Core::Esp32S3,
        }
    }

    /// The name of the architecture (and standalone platform) of the chip's
    /// code.
    pub fn arch_name(self) -> &'static str {
        self.core().arch_name()
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Chip::Esp8266 => "ESP8266",
            Chip::Esp32 => "ESP32",
            Chip::Esp32S2 => "ESP32-S2",
            Chip::Esp32S3 => "ESP32-S3",
        }
    }
}

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
    let windowed_entry = entry_code.map(|code| xtensa::is_entry_insn(&code));
    let chip = image::guess_chip(&addrs, windowed_entry);
    tracing::info!("Xtensa ELF looks like {} code", chip.display_name());
    Platform::by_name(chip.arch_name())
}

/// Registers the ESP firmware view, the SVD settings and activity, and the
/// recognizer for Xtensa ELF files. The architectures must already be
/// registered.
pub fn register() {
    svd::register_settings();
    svd::register_activity();
    register_binary_view_type(view::EspViewType);

    if let Some(elf) = BinaryViewType::by_name("ELF") {
        if let Some(esp32) = CoreArchitecture::by_name(Chip::Esp32.arch_name()) {
            elf.register_arch(ELF_MACHINE_XTENSA, Endianness::LittleEndian, &esp32);
        }
        elf.register_platform_recognizer(
            ELF_MACHINE_XTENSA,
            Endianness::LittleEndian,
            |view, _| recognize_elf(view),
        );
    }
}
