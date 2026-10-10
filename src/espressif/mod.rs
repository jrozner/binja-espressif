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

    pub fn family(self) -> Family {
        match self {
            Cpu::Xtensa(_) => Family::Xtensa,
            Cpu::RiscV(core) => match core {},
        }
    }

    /// The addresses of the functions found by scanning `code`, loaded at
    /// `addr`, for function prologues. These find code that is only reached
    /// through function pointers (callbacks, tasks, interrupt handlers),
    /// which call-target analysis cannot see.
    pub fn function_starts(self, addr: u32, code: &[u8]) -> Vec<u32> {
        match self {
            Cpu::Xtensa(core) => core.function_starts(addr, code),
            Cpu::RiscV(core) => match core {},
        }
    }

    /// Does `code` start with the usual prologue of the core's functions?
    pub fn is_prologue(self, code: &[u8]) -> bool {
        match self {
            Cpu::Xtensa(core) => core.is_prologue(code),
            Cpu::RiscV(core) => match core {},
        }
    }
}

/// An instruction set family. Chips are told apart within a family by
/// their memory maps (see [`image::guess_chip`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Xtensa,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Family::Xtensa => "Xtensa",
        }
    }

    /// The family's ELF `e_machine`.
    pub fn elf_machine(self) -> u32 {
        match self {
            Family::Xtensa => 94, // EM_XTENSA
        }
    }

    /// What the instruction at a program's entry point, `code`, says about
    /// the chip it is for.
    pub fn entry_hint(self, code: &[u8]) -> Option<EntryHint> {
        match self {
            Family::Xtensa if xtensa::is_entry_insn(code) => Some(EntryHint::Windowed),
            Family::Xtensa => Some(EntryHint::Call0),
        }
    }
}

/// The ABI of the code at a program's entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryHint {
    /// The Xtensa windowed ABI: the entry point starts with `entry`.
    Windowed,
    /// The Xtensa call0 ABI.
    Call0,
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

/// The parts of an ELF32 file that tell the chip apart.
struct ElfProgram {
    /// The load addresses of the `PT_LOAD` segments.
    addrs: Vec<u32>,
    /// The first bytes at the entry point, if it is in a segment.
    entry_code: Option<Vec<u8>>,
}

fn read_elf_program(view: &BinaryView) -> Option<ElfProgram> {
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
    let mut program = ElfProgram {
        addrs: Vec::new(),
        entry_code: None,
    };
    for i in 0..phnum.min(64) {
        let ph = view.read_vec(phoff + (i * phentsize) as u64, 32);
        if ph.len() < 32 || u32_at(&ph, 0) != 1 {
            continue; // PT_LOAD only
        }
        let (offset, vaddr, filesz) = (u32_at(&ph, 4), u32_at(&ph, 8), u32_at(&ph, 16));
        program.addrs.push(vaddr);
        if vaddr <= entry && entry < vaddr.wrapping_add(filesz) {
            program.entry_code = Some(view.read_vec((offset + entry - vaddr) as u64, 4));
        }
    }
    Some(program)
}

/// Picks the chip of an ELF file of `family` from its program headers.
fn recognize_elf(view: &BinaryView, family: Family) -> Option<Ref<Platform>> {
    let program = read_elf_program(view)?;
    let hint = program.entry_code.and_then(|code| family.entry_hint(&code));
    let chip = image::guess_chip(family, &program.addrs, hint);
    tracing::info!(
        "{} ELF looks like {} code",
        family.name(),
        chip.display_name()
    );
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
        let family = Family::Xtensa;
        if let Some(esp32) = CoreArchitecture::by_name(Chip::Esp32.arch_name()) {
            elf.register_arch(family.elf_machine(), Endianness::LittleEndian, &esp32);
        }
        elf.register_platform_recognizer(
            family.elf_machine(),
            Endianness::LittleEndian,
            move |view, _| recognize_elf(view, family),
        );
    }
}
