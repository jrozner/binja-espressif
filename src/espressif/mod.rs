//! Espressif's chips: their firmware image formats, memory maps, ROM
//! symbols and peripheral registers, and the binary view that loads their
//! firmware. Each chip's code is analyzed with the architecture of its CPU
//! core, looked up by name.

pub mod cache;
pub mod container;
pub mod elf;
pub mod image;
pub mod rom_download;
pub mod rom_images;
pub mod rom_status;
pub mod rom_symbols;
pub mod rom_types;
pub mod svd;
pub mod view;

use std::ffi::CString;

use binaryninja::Endianness;
use binaryninja::architecture::{ArchitectureExt, CoreArchitecture};
use binaryninja::binary_view::{
    BinaryView, BinaryViewBase, BinaryViewType, register_binary_view_type,
};
use binaryninja::platform::Platform;
use binaryninja::rc::Ref;
use binaryninjacore_sys::{
    BNFreeCallingConvention, BNFreePlatform, BNGetArchitectureByName,
    BNGetArchitectureCallingConventionByName, BNGetPlatformByName,
    BNRegisterPlatformCallingConvention,
};

use crate::espressif::image::{Region, RegionKind};
use crate::espressif::rom_images::RomOf;
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
        let tables = rom_symbols::of(self);
        (tables.base, tables.revisions)
    }

    /// The mask ROM symbols of an image that requires chip revision
    /// `min_rev` (major * 100 + minor) or later: those of every revision,
    /// then those of the revisions up to `min_rev`. Some functions only
    /// exist in the ROM of later revisions (the ESP32 v3.0 ROM's Secure Boot
    /// v2 API); on older chips the same addresses hold other code or
    /// nothing.
    pub fn rom_symbols_for_revision(
        self,
        min_rev: u16,
    ) -> impl Iterator<Item = &'static (&'static str, u32)> {
        let (base, revisions) = self.rom_symbols();
        std::iter::once(base)
            .chain(
                revisions
                    .iter()
                    .filter(move |&&(rev, _)| min_rev >= rev)
                    .map(|&(_, table)| table),
            )
            .flatten()
    }

    /// The chip whose platform is named `name`.
    pub fn from_platform_name(name: &str) -> Option<Chip> {
        Chip::ALL.into_iter().find(|c| c.platform_name() == name)
    }

    /// The address ranges of the mask ROM's code: those of
    /// [`Chip::rom_code`], or else the memory map's ROM region.
    pub fn rom_code_ranges(self) -> Vec<(u32, u32)> {
        match self.rom_code() {
            Some(code) => code.to_vec(),
            None => self
                .regions()
                .iter()
                .filter(|r| r.kind == RegionKind::Rom)
                .map(|r| (r.start, r.end))
                .collect(),
        }
    }

    /// The address ranges of the mask ROM's code, on the chips whose ROM
    /// code and data share an address range (from Espressif's ROM ELF
    /// files). On the others the memory map tells them apart.
    pub fn rom_code(self) -> Option<&'static [(u32, u32)]> {
        rom_symbols::of(self).code
    }

    /// Is the ROM symbol at `addr` a function (rather than data)?
    pub fn is_rom_function(self, addr: u32) -> bool {
        match self.rom_code() {
            Some(code) => code.iter().any(|&(start, end)| start <= addr && addr < end),
            None => image::region_of(self, addr).is_some_and(|r| r.kind == RegionKind::Rom),
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
            Chip::Esp32C2 => Some(svd::ESP32C2),
            Chip::Esp32C3 => Some(svd::ESP32C3),
            Chip::Esp32C6 => Some(svd::ESP32C6),
            Chip::Esp32H2 => Some(svd::ESP32H2),
            Chip::Esp32C5 => Some(svd::ESP32C5),
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
    /// The `ABI=` of an Xtensa file's `.xtensa.info` note, which the
    /// toolchain writes in every object: 0 for the windowed ABI, 1 for
    /// call0.
    xtensa_abi: Option<u32>,
}

/// `.xtensa.info`'s `ABI=` value for the call0 ABI.
const XTENSA_ABI_CALL0: u32 = 1;

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
        xtensa_abi: read_section(view, &header, ".xtensa.info")
            .and_then(|note| xtensa_info_abi(&note)),
    };
    for i in 0..phnum.min(MAX_SEGMENTS) {
        let ph = view.read_vec(phoff + (i * phentsize) as u64, 32);
        let Some(segment) = load_segment(&ph) else {
            continue;
        };
        program.segments.push((segment.vaddr, segment.memsz));
        if let Some(at) = segment.file_offset_of(entry) {
            program.entry_code = Some(view.read_vec(at as u64, 4));
        }
    }
    Some(program)
}

/// A `PT_LOAD` segment that occupies memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LoadSegment {
    offset: u32,
    vaddr: u32,
    filesz: u32,
    memsz: u32,
}

impl LoadSegment {
    /// The file offset of the contents at `addr`, if the segment has any.
    fn file_offset_of(self, addr: u32) -> Option<u32> {
        if self.vaddr <= addr && addr < self.vaddr.wrapping_add(self.filesz) {
            self.offset.checked_add(addr - self.vaddr)
        } else {
            None
        }
    }
}

/// The segment a program header describes, if it is a `PT_LOAD` segment
/// that occupies memory. An empty one says nothing about the chip:
/// Espressif's ROM ELF files have many, at address 0.
fn load_segment(ph: &[u8]) -> Option<LoadSegment> {
    const PT_LOAD: u32 = 1;
    if ph.len() < 32 || u32_at(ph, 0) != PT_LOAD {
        return None;
    }
    let segment = LoadSegment {
        offset: u32_at(ph, 4),
        vaddr: u32_at(ph, 8),
        filesz: u32_at(ph, 16),
        memsz: u32_at(ph, 20),
    };
    (segment.memsz != 0).then_some(segment)
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

/// The contents of the section named `name`, if the file has one (and it
/// is no more than 64 KiB).
fn read_section(view: &BinaryView, header: &[u8], name: &str) -> Option<Vec<u8>> {
    const MAX_SIZE: usize = 64 << 10;
    let (shoff, shentsize, shnum, shstrndx) = (
        u32_at(header, 0x20) as u64,
        u16_at(header, 0x2e),
        u16_at(header, 0x30),
        u16_at(header, 0x32),
    );
    if shoff == 0 || shentsize < 40 || shnum > MAX_SECTIONS || shstrndx >= shnum {
        return None;
    }
    let section = |i: usize| {
        let sh = view.read_vec(shoff + (i * shentsize) as u64, 40);
        (sh.len() == 40).then_some(sh)
    };
    let strtab = section(shstrndx)?;
    let (str_offset, str_size) = (u32_at(&strtab, 16) as u64, u32_at(&strtab, 20) as usize);
    if str_size > MAX_STRTAB {
        return None;
    }
    let names = view.read_vec(str_offset, str_size);
    let sh = (0..shnum).filter_map(section).find(|sh| {
        names
            .get(u32_at(sh, 0) as usize..)
            .is_some_and(|n| n.starts_with(name.as_bytes()) && n.get(name.len()) == Some(&0))
    })?;
    let (offset, size) = (u32_at(&sh, 16) as u64, u32_at(&sh, 20) as usize);
    (size <= MAX_SIZE).then(|| view.read_vec(offset, size))
}

/// The `ABI=` value in the contents of an `.xtensa.info` note (whose
/// description is lines such as `USE_ABSOLUTE_LITERALS=0` and `ABI=0`).
fn xtensa_info_abi(note: &[u8]) -> Option<u32> {
    note.split(|&c| c == b'\n' || c == 0)
        .find_map(|line| line.strip_prefix(b"ABI="))
        .and_then(|value| std::str::from_utf8(value).ok()?.trim().parse().ok())
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

/// The largest ELF file read whole to tell whether it is a ROM ELF file.
const MAX_ROM_ELF: u64 = 16 << 20;

/// Whose ROM the ELF file is, if it is one of Espressif's ROM ELF files
/// (see [`rom_images::recognize`]). Only a file that loads something into
/// the ROM code of one of the family's chips, as firmware never does, is
/// read whole to find out.
fn recognize_rom_elf(view: &BinaryView, family: Family, program: &ElfProgram) -> Option<RomOf> {
    let in_a_rom = program.segments.iter().any(|&(addr, _)| {
        Chip::ALL
            .into_iter()
            .filter(|c| c.cpu().family() == family)
            .any(|c| {
                c.rom_code_ranges()
                    .iter()
                    .any(|&(s, e)| s <= addr && addr < e)
            })
    });
    if !in_a_rom || view.len() > MAX_ROM_ELF {
        return None;
    }
    rom_images::recognize(&view.read_vec(0, view.len() as usize), family)
}

/// Picks the chip of an ELF file of `family` from its program headers and
/// its `ABS` (ROM) symbols, or returns `None` for a file that is not
/// Espressif firmware, so that it keeps Binary Ninja's own platform. One of
/// Espressif's ROM ELF files is its chip's, or keeps Binary Ninja's
/// platform if the chip is not supported.
fn recognize_elf(view: &BinaryView, family: Family) -> Option<Ref<Platform>> {
    let program = read_elf_program(view)?;
    match recognize_rom_elf(view, family, &program) {
        Some(RomOf::Chip(chip)) => {
            let path = view.file().file_path();
            let revision = path
                .file_name()
                .and_then(|n| rom_images::parse_member_name(&n.to_string_lossy()))
                .filter(|&(named, _)| named == chip)
                .map(|(_, rev)| format!(" (v{}.{})", rev / 100, rev % 100))
                .unwrap_or_default();
            tracing::info!("Espressif {} ROM ELF{revision}", chip.display_name());
            return Platform::by_name(chip.platform_name());
        }
        Some(RomOf::Other) => {
            tracing::info!(
                "{} ELF is the ROM of an Espressif chip this plugin does not support",
                family.name()
            );
            return None;
        }
        None => {}
    }
    if !is_espressif_elf(family, &program) {
        tracing::debug!("{} ELF is not Espressif firmware", family.name());
        return None;
    }
    if family == Family::Xtensa && program.segments.is_empty() {
        // A relocatable object (or a static library's member) has no
        // addresses to tell the chips apart by. Its ABI tells call0 code,
        // which only the ESP8266 runs, from the windowed code of the other
        // cores, which are taken for the most common, the ESP32.
        let chip = if program.xtensa_abi == Some(XTENSA_ABI_CALL0) {
            Chip::Esp8266
        } else {
            Chip::Esp32
        };
        tracing::info!(
            "Xtensa ELF without segments (an object?) taken for {} code",
            chip.display_name()
        );
        return Platform::by_name(chip.platform_name());
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
/// can name it.
///
/// `main` and releases build against the dev API, but the code must keep
/// building against the stable API too (`scripts/use_stable_api.sh`, CI's
/// stable-api job, the release workflow's `api: stable`). Binary Ninja 6.0
/// stable's Rust
/// API has no wrapper for this (`Platform::register_calling_convention`
/// arrived in 6.1 dev) and does not expose the platform and calling
/// convention handles, so the core is called with handles looked up by name,
/// which works on both. Switch back to `Platform::register_calling_convention`
/// once the minimum supported stable release has it.
fn add_calling_convention(platform: &str, arch: &str, cc: &str) {
    let (Ok(platform), Ok(arch), Ok(cc)) =
        (CString::new(platform), CString::new(arch), CString::new(cc))
    else {
        return;
    };
    // SAFETY: the names are valid C strings, every handle is checked for null
    // before use, and the references the lookups return are freed.
    unsafe {
        let platform = BNGetPlatformByName(platform.as_ptr());
        if platform.is_null() {
            return;
        }
        let arch = BNGetArchitectureByName(arch.as_ptr());
        if !arch.is_null() {
            let cc = BNGetArchitectureCallingConventionByName(arch, cc.as_ptr());
            if !cc.is_null() {
                BNRegisterPlatformCallingConvention(platform, cc);
                BNFreeCallingConvention(cc);
            }
        }
        BNFreePlatform(platform);
    }
}

/// Registers the chip platforms, the mask ROM type libraries (and the
/// recognizer for the ROM's functions), the ESP firmware view, the SVD settings
/// and activity, the flash dump container transform, the ELF fix-ups, and the recognizers for Xtensa and RISC-V
/// ELF files. The architectures must already be registered.
pub fn register() {
    register_platforms();
    rom_types::register_libraries();
    rom_types::register_recognizer();
    svd::register_settings();
    svd::register_activity();
    rom_download::register_settings();
    elf::register_activity();
    rom_download::register_activity();
    rom_download::register_command();
    register_binary_view_type(view::EspViewType);
    container::register();

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

    fn program_header(kind: u32, offset: u32, vaddr: u32, filesz: u32, memsz: u32) -> Vec<u8> {
        [kind, offset, vaddr, vaddr, filesz, memsz, 5, 4]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect()
    }

    #[test]
    fn empty_load_segments_are_ignored() {
        let segment = load_segment(&program_header(1, 0x100, 0x4200_0020, 0x40, 0x80)).unwrap();
        assert_eq!((segment.vaddr, segment.memsz), (0x4200_0020, 0x80));
        assert_eq!(segment.file_offset_of(0x4200_0024), Some(0x104));
        // In memory but not in the file (.bss), and outside the segment.
        assert_eq!(segment.file_offset_of(0x4200_0060), None);
        assert_eq!(segment.file_offset_of(0x4200_0000), None);
        // Espressif's ROM ELF files have many empty segments at address 0,
        // in no chip's memory map: they occupy no memory and are ignored.
        assert_eq!(load_segment(&program_header(1, 0xd8f0, 0, 0, 0)), None);
        assert_eq!(load_segment(&program_header(1, 0, 0x3ffa_e270, 0, 0)), None);
        // Only PT_LOAD segments, from whole program headers.
        assert_eq!(load_segment(&program_header(4, 0, 0x4200_0020, 4, 4)), None);
        assert_eq!(
            load_segment(&program_header(1, 0, 0x4200_0020, 4, 4)[..28]),
            None
        );
    }

    #[test]
    fn xtensa_info() {
        // The .xtensa.info note of an object built for the ESP8266.
        let note = b"\x0c\0\0\0\x20\0\0\0\x01\0\0\0Xtensa_Info\0\
            USE_ABSOLUTE_LITERALS=0\nABI=1\n\0\0";
        assert_eq!(xtensa_info_abi(note), Some(XTENSA_ABI_CALL0));
        assert_eq!(xtensa_info_abi(b"ABI=0\n"), Some(0));
        assert_eq!(xtensa_info_abi(b"USE_ABSOLUTE_LITERALS=0\n"), None);
        assert_eq!(xtensa_info_abi(b"ABI=x\n"), None);
        assert_eq!(xtensa_info_abi(b""), None);
    }

    #[test]
    fn rom_code_ranges() {
        // The Xtensa chips' ROM region; the RISC-V chips' ROM code, from
        // their ROM ELF files, short of the ROM region.
        assert_eq!(Chip::Esp32.rom_code_ranges(), [(0x4000_0000, 0x4007_0000)]);
        assert_eq!(
            Chip::Esp32S3.rom_code_ranges(),
            [(0x4000_0000, 0x4006_0000)]
        );
        assert_eq!(
            Chip::Esp32C3.rom_code_ranges(),
            [(0x4000_0000, 0x4005_99cc)]
        );
        for chip in Chip::ALL {
            let ranges = chip.rom_code_ranges();
            assert!(!ranges.is_empty(), "{chip:?}");
            for (start, end) in ranges {
                assert!(start < end, "{chip:?}");
                // Inside the ROM, and holding exactly its functions.
                let region = image::region_of(chip, start).unwrap();
                assert_eq!(region.kind, RegionKind::Rom, "{chip:?}");
                assert!(region.contains(end - 1), "{chip:?}");
                assert!(chip.is_rom_function(start) && chip.is_rom_function(end - 1));
                assert!(!chip.is_rom_function(end), "{chip:?}");
            }
        }
    }

    #[test]
    fn chips_by_platform_name() {
        for chip in Chip::ALL {
            assert_eq!(Chip::from_platform_name(chip.platform_name()), Some(chip));
        }
        assert_eq!(Chip::from_platform_name("xtensa-esp32"), None);
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
