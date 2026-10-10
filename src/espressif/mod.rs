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

use crate::espressif::image::Region;
use crate::xtensa::{self, Core};

/// Espressif's RISC-V CPU cores. None are supported yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiscVCore {}

/// The CPU core of a chip, which decides the architecture its code is
/// analyzed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cpu {
    Xtensa(Core),
    RiscV(RiscVCore),
}

impl Cpu {
    /// The name of the architecture (and standalone platform).
    pub fn arch_name(self) -> &'static str {
        match self {
            Cpu::Xtensa(core) => core.arch_name(),
            Cpu::RiscV(core) => match core {},
        }
    }

    /// Does the core's code use the Xtensa windowed ABI, whose functions
    /// start with `entry`?
    pub fn windowed(self) -> bool {
        match self {
            Cpu::Xtensa(core) => core.windowed(),
            Cpu::RiscV(core) => match core {},
        }
    }
}

/// A symbol table: names and addresses.
pub type Symbols = &'static [(&'static str, u32)];

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
    pub fn cpu(self) -> Cpu {
        Cpu::Xtensa(match self {
            Chip::Esp8266 => Core::Esp8266,
            Chip::Esp32 => Core::Esp32,
            Chip::Esp32S2 => Core::Esp32S2,
            Chip::Esp32S3 => Core::Esp32S3,
        })
    }

    /// The name of the architecture (and standalone platform) of the chip's
    /// code.
    pub fn arch_name(self) -> &'static str {
        self.cpu().arch_name()
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Chip::Esp8266 => "ESP8266",
            Chip::Esp32 => "ESP32",
            Chip::Esp32S2 => "ESP32-S2",
            Chip::Esp32S3 => "ESP32-S3",
        }
    }

    /// The chip's `chip_id` in an ESP-IDF image's extended header; ESP8266
    /// images have none.
    pub fn image_chip_id(self) -> Option<u16> {
        match self {
            Chip::Esp8266 => None,
            Chip::Esp32 => Some(0),
            Chip::Esp32S2 => Some(2),
            Chip::Esp32S3 => Some(9),
        }
    }

    /// The chip's memory map.
    pub fn regions(self) -> &'static [Region] {
        match self {
            Chip::Esp8266 => image::ESP8266_REGIONS,
            Chip::Esp32 => image::ESP32_REGIONS,
            Chip::Esp32S2 => image::ESP32S2_REGIONS,
            Chip::Esp32S3 => image::ESP32S3_REGIONS,
        }
    }

    /// The mask ROM's symbols: those of every revision of the chip, and
    /// those only in the ROM of a minimum chip revision (major * 100 +
    /// minor) and later.
    pub fn rom_symbols(self) -> (Symbols, &'static [(u16, Symbols)]) {
        use rom_symbols as r;
        match self {
            Chip::Esp8266 => (r::ESP8266, r::ESP8266_REVISIONS),
            Chip::Esp32 => (r::ESP32, r::ESP32_REVISIONS),
            Chip::Esp32S2 => (r::ESP32S2, r::ESP32S2_REVISIONS),
            Chip::Esp32S3 => (r::ESP32S3, r::ESP32S3_REVISIONS),
        }
    }

    /// The gzip-compressed Espressif SVD file for the chip; the ESP8266 has
    /// none.
    pub fn bundled_svd(self) -> Option<&'static [u8]> {
        match self {
            Chip::Esp8266 => None,
            Chip::Esp32 => Some(svd::ESP32),
            Chip::Esp32S2 => Some(svd::ESP32S2),
            Chip::Esp32S3 => Some(svd::ESP32S3),
        }
    }

    /// The name of the entry point of the chip's firmware images.
    pub fn entry_symbol_name(self) -> &'static str {
        match self {
            Chip::Esp8266 => "call_user_start",
            _ => "call_start_cpu0",
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
