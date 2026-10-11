//! Espressif's chips: their firmware image formats, memory maps, ROM
//! symbols and peripheral registers, and the binary view that loads their
//! firmware. Each chip's code is analyzed with the architecture of its CPU
//! core, looked up by name.

pub mod elf;
pub mod image;
pub mod rom_symbols;
pub mod svd;
pub mod view;

use std::ffi::CString;

use binaryninja::Endianness;
use binaryninja::architecture::{ArchitectureExt, CoreArchitecture};
use binaryninja::binary_view::{BinaryView, BinaryViewType, register_binary_view_type};
use binaryninja::platform::Platform;
use binaryninja::rc::Ref;
use binaryninjacore_sys::{
    BNFreeCallingConvention, BNFreePlatform, BNGetArchitectureByName,
    BNGetArchitectureCallingConventionByName, BNGetPlatformByName,
    BNRegisterPlatformCallingConvention,
};

use crate::espressif::image::{Region, RegionKind};
use crate::riscv;
use crate::xtensa::{self, Core};

/// The CPU core of a chip, which decides the architecture its code is
/// analyzed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cpu {
    Xtensa(Core),
    RiscV(riscv::Core),
}

impl Cpu {
    /// The name of the architecture (and standalone platform).
    pub fn arch_name(self) -> &'static str {
        match self {
            Cpu::Xtensa(core) => core.arch_name(),
            Cpu::RiscV(_) => riscv::ARCH_NAME,
        }
    }

    /// Does the core's code use the Xtensa windowed ABI, whose functions
    /// start with `entry`?
    pub fn windowed(self) -> bool {
        match self {
            Cpu::Xtensa(core) => core.windowed(),
            Cpu::RiscV(_) => false,
        }
    }

    pub fn family(self) -> Family {
        match self {
            Cpu::Xtensa(_) => Family::Xtensa,
            Cpu::RiscV(_) => Family::RiscV,
        }
    }

    /// The name of the default calling convention of the chip platforms.
    /// The Xtensa cores' is the architecture's own; on the RISC-V cores it
    /// is the soft-float `ilp32` rather than `rv32gc`'s hard-float default.
    pub fn default_calling_convention(self) -> Option<&'static str> {
        match self {
            Cpu::Xtensa(_) => None,
            Cpu::RiscV(_) => Some(riscv::ILP32),
        }
    }

    /// What scanning `code`, loaded at `addr`, finds: functions that are
    /// only reached through function pointers (callbacks, tasks, interrupt
    /// handlers), which call-target analysis cannot see. On the Xtensa
    /// cores, the windowed functions' `entry` instructions; on the RISC-V
    /// cores, the interrupt vector tables and their handlers.
    pub fn scan_code(self, addr: u32, code: &[u8]) -> CodeScan {
        let mut scan = CodeScan::default();
        match self {
            Cpu::Xtensa(core) => {
                scan.functions = core
                    .function_starts(addr, code)
                    .into_iter()
                    .map(|a| (a, None))
                    .collect();
            }
            Cpu::RiscV(_) => {
                for table in riscv::vector_tables(addr, code) {
                    match table.kind {
                        // The table is code: entry n is a jump taken for cause n.
                        riscv::VectorTableKind::Jumps => {
                            scan.functions.push((table.addr, Some(table.name())));
                        }
                        riscv::VectorTableKind::Pointers => {
                            scan.pointer_tables
                                .push((table.addr, table.name(), table.entries()));
                        }
                    }
                    scan.functions.extend(table.handlers);
                }
            }
        }
        scan
    }

    /// The value of the global pointer register set by the code at the
    /// entry point, `code`, loaded at `addr`.
    pub fn global_pointer(self, addr: u32, code: &[u8]) -> Option<u32> {
        match self {
            Cpu::Xtensa(_) => None,
            Cpu::RiscV(_) => riscv::global_pointer(addr, code),
        }
    }

    /// Does `code` start with the usual prologue of the core's functions?
    pub fn is_prologue(self, code: &[u8]) -> bool {
        match self {
            Cpu::Xtensa(core) => core.is_prologue(code),
            Cpu::RiscV(core) => core.is_prologue(code),
        }
    }
}

/// What [`Cpu::scan_code`] found in a code segment.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CodeScan {
    /// Function starts, with their names where they are known.
    pub functions: Vec<(u32, Option<&'static str>)>,
    /// Tables of code addresses: address, name and number of entries.
    pub pointer_tables: Vec<(u32, &'static str, usize)>,
}

/// An instruction set family. Chips are told apart within a family by
/// their memory maps (see [`image::guess_chip`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Xtensa,
    RiscV,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Family::Xtensa => "Xtensa",
            Family::RiscV => "RISC-V",
        }
    }

    /// The family's ELF `e_machine`.
    pub fn elf_machine(self) -> u32 {
        match self {
            Family::Xtensa => 94, // EM_XTENSA
            Family::RiscV => 243, // EM_RISCV
        }
    }

    /// What the instruction at a program's entry point, `code`, says about
    /// the chip it is for.
    pub fn entry_hint(self, code: &[u8]) -> Option<EntryHint> {
        match self {
            Family::Xtensa if xtensa::is_entry_insn(code) => Some(EntryHint::Windowed),
            Family::Xtensa => Some(EntryHint::Call0),
            // ESP-IDF's RISC-V entry points all start the same way.
            Family::RiscV => None,
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
    Esp32C2,
    Esp32C3,
    Esp32C6,
    Esp32H2,
    Esp32C5,
}

impl Chip {
    pub const ALL: [Chip; 9] = [
        Chip::Esp8266,
        Chip::Esp32,
        Chip::Esp32S2,
        Chip::Esp32S3,
        Chip::Esp32C2,
        Chip::Esp32C3,
        Chip::Esp32C6,
        Chip::Esp32H2,
        Chip::Esp32C5,
    ];

    /// The chip's CPU core.
    pub fn cpu(self) -> Cpu {
        match self {
            Chip::Esp8266 => Cpu::Xtensa(Core::Esp8266),
            Chip::Esp32 => Cpu::Xtensa(Core::Esp32),
            Chip::Esp32S2 => Cpu::Xtensa(Core::Esp32S2),
            Chip::Esp32S3 => Cpu::Xtensa(Core::Esp32S3),
            Chip::Esp32C2 | Chip::Esp32C3 => Cpu::RiscV(riscv::Core::Rv32imc),
            Chip::Esp32C6 | Chip::Esp32H2 | Chip::Esp32C5 => Cpu::RiscV(riscv::Core::Rv32imac),
        }
    }

    /// The name of the architecture of the chip's code.
    pub fn arch_name(self) -> &'static str {
        self.cpu().arch_name()
    }

    /// The name of the chip's platform: the chip's ESP-IDF target name.
    /// Chips with the same CPU core share an architecture, so the platform,
    /// not the architecture, says which chip a view is for.
    pub fn platform_name(self) -> &'static str {
        match self {
            Chip::Esp8266 => "esp8266",
            Chip::Esp32 => "esp32",
            Chip::Esp32S2 => "esp32s2",
            Chip::Esp32S3 => "esp32s3",
            Chip::Esp32C2 => "esp32c2",
            Chip::Esp32C3 => "esp32c3",
            Chip::Esp32C6 => "esp32c6",
            Chip::Esp32H2 => "esp32h2",
            Chip::Esp32C5 => "esp32c5",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Chip::Esp8266 => "ESP8266",
            Chip::Esp32 => "ESP32",
            Chip::Esp32S2 => "ESP32-S2",
            Chip::Esp32S3 => "ESP32-S3",
            Chip::Esp32C2 => "ESP32-C2",
            Chip::Esp32C3 => "ESP32-C3",
            Chip::Esp32C6 => "ESP32-C6",
            Chip::Esp32H2 => "ESP32-H2",
            Chip::Esp32C5 => "ESP32-C5",
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
            Chip::Esp32C2 => Some(12),
            Chip::Esp32C3 => Some(5),
            Chip::Esp32C6 => Some(13),
            Chip::Esp32H2 => Some(16),
            Chip::Esp32C5 => Some(23),
        }
    }

    /// The minimum chip revision (major * 100 + minor) in the legacy
    /// `min_chip_rev` byte of an ESP-IDF image header, for images without
    /// `min_chip_rev_full`. ESP-IDF's `esp_app_format.h`: the byte is "a
    /// Major version for most of the chips and [...] a Minor version for the
    /// ESP32-C3".
    pub fn legacy_chip_rev(self, byte: u8) -> u16 {
        match self {
            Chip::Esp32C3 => byte as u16,
            _ => byte as u16 * 100,
        }
    }

    /// The chip's memory map.
    pub fn regions(self) -> &'static [Region] {
        match self {
            Chip::Esp8266 => image::ESP8266_REGIONS,
            Chip::Esp32 => image::ESP32_REGIONS,
            Chip::Esp32S2 => image::ESP32S2_REGIONS,
            Chip::Esp32S3 => image::ESP32S3_REGIONS,
            Chip::Esp32C2 => image::ESP32C2_REGIONS,
            Chip::Esp32C3 => image::ESP32C3_REGIONS,
            Chip::Esp32C6 => image::ESP32C6_REGIONS,
            Chip::Esp32H2 => image::ESP32H2_REGIONS,
            Chip::Esp32C5 => image::ESP32C5_REGIONS,
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
            Chip::Esp32C2 => (r::ESP32C2, r::ESP32C2_REVISIONS),
            Chip::Esp32C3 => (r::ESP32C3, r::ESP32C3_REVISIONS),
            Chip::Esp32C6 => (r::ESP32C6, r::ESP32C6_REVISIONS),
            Chip::Esp32H2 => (r::ESP32H2, r::ESP32H2_REVISIONS),
            Chip::Esp32C5 => (r::ESP32C5, r::ESP32C5_REVISIONS),
        }
    }

    /// The address ranges of the mask ROM's code, on the chips whose ROM
    /// code and data share an address range (from Espressif's ROM ELF
    /// files). On the others the memory map tells them apart.
    pub fn rom_code(self) -> Option<&'static [(u32, u32)]> {
        use rom_symbols as r;
        match self {
            Chip::Esp8266 | Chip::Esp32 | Chip::Esp32S2 | Chip::Esp32S3 => None,
            Chip::Esp32C2 => Some(r::ESP32C2_ROM_CODE),
            Chip::Esp32C3 => Some(r::ESP32C3_ROM_CODE),
            Chip::Esp32C6 => Some(r::ESP32C6_ROM_CODE),
            Chip::Esp32H2 => Some(r::ESP32H2_ROM_CODE),
            Chip::Esp32C5 => Some(r::ESP32C5_ROM_CODE),
        }
    }

    /// Is the ROM symbol at `addr` a function (rather than data)?
    pub fn is_rom_function(self, addr: u32) -> bool {
        match self.rom_code() {
            Some(code) => code.iter().any(|&(start, end)| start <= addr && addr < end),
            None => image::region_of(self, addr).is_some_and(|r| r.kind == RegionKind::Rom),
        }
    }

    /// The gzip-compressed Espressif SVD file for the chip; the ESP8266 and
    /// the ESP32-C5 have none.
    pub fn bundled_svd(self) -> Option<&'static [u8]> {
        match self {
            Chip::Esp8266 | Chip::Esp32C5 => None,
            Chip::Esp32 => Some(svd::ESP32),
            Chip::Esp32S2 => Some(svd::ESP32S2),
            Chip::Esp32S3 => Some(svd::ESP32S3),
            Chip::Esp32C2 => Some(svd::ESP32C2),
            Chip::Esp32C3 => Some(svd::ESP32C3),
            Chip::Esp32C6 => Some(svd::ESP32C6),
            Chip::Esp32H2 => Some(svd::ESP32H2),
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

/// The parts of a little-endian ELF32 file that tell the chip apart.
struct ElfProgram {
    /// `e_flags`.
    flags: u32,
    /// The address and size in memory of the `PT_LOAD` segments.
    segments: Vec<(u32, u32)>,
    /// The first bytes at the entry point, if it is in a segment.
    entry_code: Option<Vec<u8>>,
    /// The names and values of the `ABS` symbols in the symbol table.
    abs_symbols: Vec<(String, u32)>,
}

/// Limits on what is read from a (possibly malformed) ELF file.
const MAX_SEGMENTS: usize = 64;
const MAX_SECTIONS: usize = 4096;
const MAX_SYMBOLS: usize = 1 << 20;
const MAX_STRTAB: usize = 64 << 20;

fn u16_at(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes([b[o], b[o + 1]]) as usize
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn read_elf_program(view: &BinaryView) -> Option<ElfProgram> {
    let header = view.read_vec(0, 0x34);
    if header.len() < 0x34 || header[4] != 1 || header[5] != 1 {
        return None; // not little-endian ELF32
    }
    let entry = u32_at(&header, 0x18);
    let (phoff, phentsize, phnum) = (
        u32_at(&header, 0x1c) as u64,
        u16_at(&header, 0x2a),
        u16_at(&header, 0x2c),
    );
    let mut program = ElfProgram {
        flags: u32_at(&header, 0x24),
        segments: Vec::new(),
        entry_code: None,
        abs_symbols: read_abs_symbols(view, &header),
    };
    for i in 0..phnum.min(MAX_SEGMENTS) {
        let ph = view.read_vec(phoff + (i * phentsize) as u64, 32);
        if ph.len() < 32 || u32_at(&ph, 0) != 1 {
            continue; // PT_LOAD only
        }
        let (offset, vaddr, filesz, memsz) = (
            u32_at(&ph, 4),
            u32_at(&ph, 8),
            u32_at(&ph, 16),
            u32_at(&ph, 20),
        );
        program.segments.push((vaddr, memsz));
        if vaddr <= entry && entry < vaddr.wrapping_add(filesz) {
            program.entry_code = Some(view.read_vec((offset + entry - vaddr) as u64, 4));
        }
    }
    Some(program)
}

/// The `ABS` symbols of the ELF file's symbol table (`SHT_SYMTAB`), or none
/// if it has none or it is malformed.
fn read_abs_symbols(view: &BinaryView, header: &[u8]) -> Vec<(String, u32)> {
    const SHT_SYMTAB: u32 = 2;
    const SHN_ABS: usize = 0xfff1;
    let (shoff, shentsize, shnum) = (
        u32_at(header, 0x20) as u64,
        u16_at(header, 0x2e),
        u16_at(header, 0x30),
    );
    if shoff == 0 || shentsize < 40 || shnum > MAX_SECTIONS {
        return Vec::new();
    }
    let section = |i: usize| {
        let sh = view.read_vec(shoff + (i * shentsize) as u64, 40);
        (sh.len() == 40).then_some(sh)
    };
    let Some(symtab) = (0..shnum)
        .filter_map(section)
        .find(|sh| u32_at(sh, 4) == SHT_SYMTAB)
    else {
        return Vec::new();
    };
    let (offset, size, link) = (
        u32_at(&symtab, 16) as u64,
        u32_at(&symtab, 20) as usize,
        u32_at(&symtab, 24) as usize,
    );
    let Some(strtab) = (link < shnum).then(|| section(link)).flatten() else {
        return Vec::new();
    };
    let (str_offset, str_size) = (u32_at(&strtab, 16) as u64, u32_at(&strtab, 20) as usize);
    if size / 16 > MAX_SYMBOLS || str_size > MAX_STRTAB {
        return Vec::new();
    }
    let symbols = view.read_vec(offset, size);
    let names = view.read_vec(str_offset, str_size);
    symbols
        .as_chunks::<16>()
        .0
        .iter()
        .filter(|sym| u16_at(*sym, 14) == SHN_ABS)
        .filter_map(|sym| {
            let name = names.get(u32_at(sym, 0) as usize..)?;
            let name = &name[..name.iter().position(|&c| c == 0)?];
            (!name.is_empty()).then(|| (String::from_utf8_lossy(name).into_owned(), u32_at(sym, 4)))
        })
        .collect()
}

/// `EF_RISCV_FLOAT_ABI`: the float ABI bits of a RISC-V ELF's `e_flags`;
/// zero for the soft-float ABI that all supported Espressif chips use.
const EF_RISCV_FLOAT_ABI: u32 = 0x6;
/// `EF_RISCV_RVE`: the RV32E base ISA.
const EF_RISCV_RVE: u32 = 0x8;

/// Is the ELF file plausibly firmware for one of the family's chips? Xtensa
/// files always are (Espressif's are the only Xtensa cores supported). A
/// RISC-V file must use the soft-float ilp32 ABI and load only into a
/// supported chip's memory.
fn is_espressif_elf(family: Family, program: &ElfProgram) -> bool {
    match family {
        Family::Xtensa => true,
        Family::RiscV => {
            program.flags & (EF_RISCV_FLOAT_ABI | EF_RISCV_RVE) == 0
                && !program.segments.is_empty()
                && Chip::ALL.into_iter().any(|chip| {
                    chip.cpu().family() == family && image::fits_memory_map(chip, &program.segments)
                })
        }
    }
}

/// Picks the chip of an ELF file of `family` from its program headers and
/// its `ABS` (ROM) symbols, or returns `None` for a file that is not
/// Espressif firmware, so that it keeps Binary Ninja's own platform.
fn recognize_elf(view: &BinaryView, family: Family) -> Option<Ref<Platform>> {
    let program = read_elf_program(view)?;
    if !is_espressif_elf(family, &program) {
        tracing::debug!("{} ELF is not Espressif firmware", family.name());
        return None;
    }
    let hint = program
        .entry_code
        .as_deref()
        .and_then(|code| family.entry_hint(code));
    let (chip, matches) =
        image::guess_elf_chip(family, &program.segments, hint, &program.abs_symbols);
    tracing::info!(
        "{} ELF looks like {} code ({matches} ROM symbols match)",
        family.name(),
        chip.display_name()
    );
    Platform::by_name(chip.platform_name())
}

/// The OS name the chip platforms are registered under.
const PLATFORM_OS: &str = "espressif";

/// Registers a platform for every chip on its CPU's architecture, with the
/// architecture's calling conventions. The default calling convention is
/// the CPU's (see [`Cpu::default_calling_convention`]), or else that of the
/// architecture's standalone platform.
fn register_platforms() {
    for chip in Chip::ALL {
        let Some(arch) = CoreArchitecture::by_name(chip.arch_name()) else {
            // Binary Ninja's RISC-V architecture plugin may be disabled.
            tracing::warn!(
                "architecture {} is not registered; {} is not supported",
                chip.arch_name(),
                chip.display_name()
            );
            continue;
        };
        let platform = Platform::new(&arch, chip.platform_name());
        platform.register_os(PLATFORM_OS);
        for cc in arch.calling_conventions().iter() {
            add_calling_convention(chip.platform_name(), chip.arch_name(), &cc.name());
        }
        let default = match chip.cpu().default_calling_convention() {
            Some(name) => arch.calling_convention_by_name(name),
            None => arch
                .standalone_platform()
                .and_then(|p| p.get_default_calling_convention()),
        };
        if let Some(cc) = default {
            platform.set_default_calling_convention(&cc);
        }
    }
}

/// Adds an architecture's calling convention to a platform, so that types
/// can name it. The Rust API has no wrapper for this, and does not expose
/// the platform and calling convention handles, so the core is called with
/// handles looked up by name.
fn add_calling_convention(platform: &str, arch: &str, cc: &str) {
    let (platform, arch, cc) = (
        CString::new(platform).unwrap(),
        CString::new(arch).unwrap(),
        CString::new(cc).unwrap(),
    );
    unsafe {
        let platform = BNGetPlatformByName(platform.as_ptr());
        let arch = BNGetArchitectureByName(arch.as_ptr());
        if platform.is_null() || arch.is_null() {
            return;
        }
        let cc = BNGetArchitectureCallingConventionByName(arch, cc.as_ptr());
        if !cc.is_null() {
            BNRegisterPlatformCallingConvention(platform, cc);
            BNFreeCallingConvention(cc);
        }
        BNFreePlatform(platform);
    }
}

/// Registers the chip platforms, the ESP firmware view, the SVD settings
/// and activity, the ELF fix-ups, and the recognizers for Xtensa and RISC-V
/// ELF files. The architectures must already be registered.
pub fn register() {
    register_platforms();
    svd::register_settings();
    svd::register_activity();
    elf::register_activity();
    register_binary_view_type(view::EspViewType);

    if let Some(elf) = BinaryViewType::by_name("ELF") {
        // EM_XTENSA has no architecture of its own; EM_RISCV belongs to
        // Binary Ninja's RISC-V plugin, whose platforms are kept for RISC-V
        // files that are not Espressif firmware.
        if let Some(esp32) = CoreArchitecture::by_name(Chip::Esp32.arch_name()) {
            elf.register_arch(
                Family::Xtensa.elf_machine(),
                Endianness::LittleEndian,
                &esp32,
            );
        }
        for family in [Family::Xtensa, Family::RiscV] {
            elf.register_platform_recognizer(
                family.elf_machine(),
                Endianness::LittleEndian,
                move |view, _| recognize_elf(view, family),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_symbol(chip: Chip, name: &str) -> u32 {
        let (base, revisions) = chip.rom_symbols();
        let tables = std::iter::once(base).chain(revisions.iter().map(|&(_, t)| t));
        tables
            .flatten()
            .find(|&&(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is not a {} ROM symbol", chip.display_name()))
            .1
    }

    #[test]
    fn rom_symbol_kinds() {
        for chip in Chip::ALL {
            let f = |name| chip.is_rom_function(rom_symbol(chip, name));
            assert!(f("memcpy"), "{chip:?} memcpy");
            assert!(f("ets_printf"), "{chip:?} ets_printf");
        }
        // Data next to the code in the RISC-V ROMs' address range, and in
        // the ROM's RAM.
        for chip in [Chip::Esp32C6, Chip::Esp32C5] {
            assert!(!chip.is_rom_function(rom_symbol(chip, "he_max_apep_length")));
            assert!(!chip.is_rom_function(rom_symbol(chip, "g_coex_param_ptr")));
        }
        assert!(!Chip::Esp32H2.is_rom_function(rom_symbol(Chip::Esp32H2, "crc8_be_table_ptr")));
        assert!(!Chip::Esp32C2.is_rom_function(rom_symbol(Chip::Esp32C2, "crc8_be_table_ptr")));
        assert!(!Chip::Esp32C3.is_rom_function(rom_symbol(Chip::Esp32C3, "g_coex_param_ptr")));
        // The Xtensa chips go by region.
        assert!(!Chip::Esp32.is_rom_function(rom_symbol(Chip::Esp32, "_ctype_")));
        assert!(!Chip::Esp32S2.is_rom_function(rom_symbol(Chip::Esp32S2, "_ctype_")));
    }

    #[test]
    fn rom_code_is_in_the_rom() {
        for chip in Chip::ALL {
            let Some(code) = chip.rom_code() else {
                continue;
            };
            assert!(!code.is_empty());
            for &(start, end) in code {
                let region = image::region_of(chip, start).unwrap();
                assert_eq!(region.kind, RegionKind::Rom, "{chip:?} {start:#x}");
                assert!(region.contains(end - 1), "{chip:?} {end:#x}");
            }
            // ROM functions are only ever in the ROM.
            let (base, revisions) = chip.rom_symbols();
            let tables = std::iter::once(base).chain(revisions.iter().map(|&(_, t)| t));
            for &(name, addr) in tables.flatten() {
                if chip.is_rom_function(addr) {
                    let region = image::region_of(chip, addr).map(|r| r.kind);
                    assert_eq!(region, Some(RegionKind::Rom), "{chip:?} {name}");
                }
            }
        }
    }
}
