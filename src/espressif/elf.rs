//! Fix-ups for ELF files of Espressif chips (ESP-IDF and SDK builds), so
//! that they analyze like the ESP view's firmware images.
//!
//! Binary Ninja's ELF view loads these files on a chip platform (see
//! `recognize_elf`), but misses three things the ESP view provides:
//!
//! * **Read-only flash data.** ESP-IDF's linker scripts emit `.flash.rodata`
//!   as writable (`WA`, in an `RW` segment), so Binary Ninja does not read
//!   jump tables or constants from it. Sections in the chip's read-only
//!   flash data regions are redefined as read-only data.
//! * **Mask ROM symbols.** The ROM functions an application calls are `ABS`
//!   symbols, but the ROM is not in any segment and the ELF view drops them.
//!   They are defined at their (unmapped) addresses, as in the ESP view,
//!   together with the bundled ROM table for names the ELF does not carry,
//!   and typed from the chip's ROM type library (see `rom_types`).
//! * **The global pointer** of RISC-V code, from `__global_pointer$`.
//!
//! These run as an analysis step at the very start of module analysis,
//! before any function is analyzed; the view's finalization event fires too
//! late for a new file, when its functions have been analyzed with the
//! writable `.flash.rodata`. A database, however, is reopened without that
//! step running again, while the ELF view recreates its sections as the file
//! describes them, so the fix-ups also run on finalization. Every change
//! checks what is already there, so running them twice changes nothing (the
//! ROM types, which a database keeps, are left alone there).

use binaryninja::binary_view::{
    BinaryView, BinaryViewBase, BinaryViewEventType, register_binary_view_event,
};
use binaryninja::section::{Section, SectionBuilder, Semantics};
use binaryninja::symbol::{SymbolBuilder, SymbolType};
use binaryninja::workflow::{Activity, AnalysisContext, Workflow, activity};

use crate::espressif::image::{self, RegionKind};
use crate::espressif::rom_images::{self, RomElf, RomOf};
use crate::espressif::{Chip, Cpu, rom_types};
use crate::riscv;

/// The first step of module analysis, which the fix-ups run before.
pub const FIRST_STEP: &str = "core.module.loadDebugInfo";

/// The chip of an ELF view on one of the chip platforms.
fn chip_of(view: &BinaryView) -> Option<Chip> {
    if view.view_type() != "ELF" {
        return None;
    }
    let name = view.default_platform()?.name();
    Chip::from_platform_name(&name)
}

/// Registers the fix-up step at the start of module analysis, and the
/// fix-ups on view finalization (for reopened databases).
pub fn register_activity() {
    register_binary_view_event(
        BinaryViewEventType::BinaryViewFinalizationEvent,
        |view: &BinaryView| {
            if let Some(chip) = chip_of(view) {
                apply(view, chip);
            }
        },
    );
    let config = activity::Config::action(
        "espressif.elf.fixups",
        "Espressif ELF fix-ups",
        "Maps ESP-IDF's flash data read-only and defines the mask ROM's symbols and the global pointer for ELF files of Espressif chips.",
    )
    .eligibility(activity::Eligibility::auto().run_once(true));
    let fixups = Activity::new_with_action(config, |ctx: &AnalysisContext| {
        let view = ctx.view();
        if let Some(chip) = chip_of(&view) {
            apply(&view, chip);
        }
    });
    let registered = Workflow::cloned("core.module.metaAnalysis")
        .ok_or(())
        .and_then(|w| w.activity_before(&fixups, FIRST_STEP))
        .and_then(|w| w.register());
    if registered.is_err() {
        tracing::error!("cannot register the Espressif ELF fix-ups");
    }
}

fn apply(view: &BinaryView, chip: Chip) {
    let raw = view.parent_view();
    let file = raw
        .map(|r| r.read_vec(0, r.len() as usize))
        .unwrap_or_default();
    let symbols = elf_symbols(&file);
    make_flash_data_read_only(view, chip);
    define_rom_symbols(view, chip, &symbols);
    if let Some(rom) = own_rom(view, chip, &file) {
        rom_types::type_rom_elf(view, chip, &rom);
    }
    if let Cpu::RiscV(_) = chip.cpu()
        && let Some(gp) = symbols.iter().find(|s| s.name == "__global_pointer$")
    {
        riscv::set_global_pointer(view, gp.value);
    }
}

/// The chip's ROM, if the file is the chip's ROM ELF file (see
/// [`rom_images::recognize`]). Only a view with a segment in the ROM's code,
/// which firmware never has, is checked.
fn own_rom(view: &BinaryView, chip: Chip, file: &[u8]) -> Option<RomElf> {
    let ranges = chip.rom_code_ranges();
    let in_rom = view.segments().iter().any(|s| {
        let r = s.address_range();
        ranges
            .iter()
            .any(|&(start, end)| r.start < end as u64 && (start as u64) < r.end)
    });
    if !in_rom || rom_images::recognize(file, chip.cpu().family()) != Some(RomOf::Chip(chip)) {
        return None;
    }
    RomElf::parse(file, chip).ok()
}

/// Redefines the writable sections that lie in the chip's read-only flash
/// data (DROM, or the flash window shared with code) as read-only data.
/// Code sections in a shared window are left alone, and so is anything that
/// is not flash contents (see [`is_flash_contents`]).
fn make_flash_data_read_only(view: &BinaryView, chip: Chip) {
    let writable: Vec<_> = view
        .sections()
        .iter()
        .filter(|s| {
            matches!(
                s.semantics(),
                Semantics::ReadWriteData | Semantics::DefaultSection
            ) && !s.is_empty()
                && is_flash_contents(&s.name().to_string_lossy(), &s.section_type())
                && in_rodata_region(chip, s.start(), s.end())
        })
        .map(|s| s.to_owned())
        .collect();
    for section in writable {
        let name = section.name().to_string_lossy().into_owned();
        tracing::info!("{name} is flash data: making it read-only");
        view.remove_auto_section(name.as_str());
        view.add_section(read_only_copy(&section, name));
    }
}

/// Is an ELF section (by name and type) data the image puts in flash?
///
/// The C5 maps PSRAM in the same window as flash, and ESP-IDF places
/// `.ext_ram.bss` and `.ext_ram_noinit` there: mutable memory that must
/// stay writable. Everything ESP-IDF puts in external RAM is `NOLOAD`
/// (`SHT_NOBITS`), as are the flash window's placeholders
/// (`.flash_rodata_dummy`, `.flash.tbss`, `.flash.rodata_noload`), while
/// flash data always has contents in the file. External-RAM sections are
/// also excluded by name, in case one ever does have contents.
fn is_flash_contents(name: &str, section_type: &str) -> bool {
    section_type != "NOBITS" && !name.starts_with(".ext_ram")
}

fn in_rodata_region(chip: Chip, start: u64, end: u64) -> bool {
    let (Ok(start), Ok(last)) = (u32::try_from(start), u32::try_from(end - 1)) else {
        return false;
    };
    image::region_of(chip, start).is_some_and(|r| r.is_rodata() && r.contains(last))
}

fn read_only_copy(section: &Section, name: String) -> SectionBuilder {
    SectionBuilder::new(name, section.address_range())
        .semantics(Semantics::ReadOnlyData)
        .section_type(section.section_type())
        .align(section.align())
        .entry_size(section.entry_size() as u64)
        .linked_section(section.linked_section().to_string_lossy().into_owned())
        .info_section(section.info_section().to_string_lossy().into_owned())
        .info_data(section.info_data())
        .is_auto(true)
}

/// Defines the mask ROM's symbols: the ELF's own `ABS` symbols for the ROM
/// (which the ELF view drops, as no segment holds their addresses), then
/// the bundled table for the names the ELF does not define. An ELF does not
/// say which chip revision it needs, so only the symbols of every revision
/// are taken from the table; an application linked against a later
/// revision's ROM carries those as `ABS` symbols itself.
///
/// An `ABS` symbol is taken as a ROM symbol if it is in the ROM, if its
/// name and address are in one of the chip's tables (of any revision: ROM
/// variables live in RAM, and the ELF says this one exists), or if its
/// address is in the every-revision table (the same ROM function or
/// variable under another name).
///
/// Addresses that already have a symbol, and table names the ELF defines
/// elsewhere (an application's own `memcpy`, say), are left alone.
fn define_rom_symbols(view: &BinaryView, chip: Chip, symbols: &[ElfSymbol]) {
    let (table, _) = chip.rom_symbols();
    let every_revision = image::rom_symbol_set(chip);
    let mut defined = 0;
    let mut define = |name: &str, addr: u32| {
        if view.symbol_by_address(addr as u64).is_some() {
            return;
        }
        let kind = if chip.is_rom_function(addr) {
            SymbolType::ImportedFunction
        } else {
            SymbolType::Data
        };
        view.define_auto_symbol(&SymbolBuilder::new(kind, name, addr as u64).create());
        defined += 1;
    };
    // The names to type, by preference: the ROM tables' names, then the
    // other names the ELF gives ROM addresses (esp_rom API aliases).
    let mut tabled = Vec::new();
    let mut aliases = Vec::new();
    for s in symbols.iter().filter(|s| s.absolute && !s.name.is_empty()) {
        let named_in_table = every_revision.contains(&(s.name.as_str(), s.value));
        let in_rom_table = table.iter().any(|&(_, addr)| addr == s.value);
        if named_in_table || in_rom_table || is_rom(chip, s.value) {
            define(&s.name, s.value);
            let list = if named_in_table {
                &mut tabled
            } else {
                &mut aliases
            };
            list.push((s.name.as_str(), s.value));
        }
    }
    for &(name, addr) in table {
        if view.symbol_by_raw_name(name).is_none() {
            define(name, addr);
        }
    }
    tracing::info!("defined {defined} mask ROM symbols");
    // Not the table names the ELF defines elsewhere.
    let table = table.iter().copied().filter(|&(name, addr)| {
        view.symbol_by_raw_name(name)
            .is_some_and(|s| s.address() == addr as u64)
    });
    rom_types::apply(view, chip, tabled.into_iter().chain(table).chain(aliases));
}

fn is_rom(chip: Chip, addr: u32) -> bool {
    image::region_of(chip, addr)
        .is_some_and(|r| matches!(r.kind, RegionKind::Rom | RegionKind::RomData))
}

/// A symbol of an ELF32 file's symbol table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfSymbol {
    pub name: String,
    pub value: u32,
    /// Defined with `SHN_ABS` (a linker-script address).
    pub absolute: bool,
    /// A function (`STT_FUNC`).
    pub function: bool,
}

/// The symbols of a little-endian ELF32 file's `SHT_SYMTAB`, or none if it
/// has none or is malformed.
pub fn elf_symbols(file: &[u8]) -> Vec<ElfSymbol> {
    const SHT_SYMTAB: u32 = 2;
    const SHN_ABS: u16 = 0xfff1;
    const STT_FUNC: u8 = 2;
    let u16_at = |o: usize| file.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| {
        file.get(o..o + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let section = |i: usize| -> Option<(u32, usize, usize, usize)> {
        let shoff = u32_at(0x20)? as usize;
        let shentsize = u16_at(0x2e)? as usize;
        let h = shoff.checked_add(i.checked_mul(shentsize)?)?;
        Some((
            u32_at(h + 4)?,
            u32_at(h + 16)? as usize,
            u32_at(h + 20)? as usize,
            u32_at(h + 24)? as usize,
        ))
    };
    if file.get(0..5) != Some(b"\x7fELF\x01") || file.get(5) != Some(&1) {
        return Vec::new();
    }
    let Some(shnum) = u16_at(0x30) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..shnum as usize {
        let Some((kind, offset, size, link)) = section(i) else {
            break;
        };
        if kind != SHT_SYMTAB {
            continue;
        }
        let Some((_, strtab, strsize, _)) = section(link) else {
            continue;
        };
        let strings = file
            .get(strtab..strtab.saturating_add(strsize))
            .unwrap_or(&[]);
        for sym in file
            .get(offset..offset.saturating_add(size))
            .unwrap_or(&[])
            .as_chunks::<16>()
            .0
        {
            let name_off = u32::from_le_bytes(sym[0..4].try_into().unwrap()) as usize;
            let name = strings
                .get(name_off..)
                .and_then(|s| s.split(|&c| c == 0).next())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .unwrap_or_default();
            out.push(ElfSymbol {
                name,
                value: u32::from_le_bytes(sym[4..8].try_into().unwrap()),
                absolute: u16::from_le_bytes([sym[14], sym[15]]) == SHN_ABS,
                function: sym[12] & 0xf == STT_FUNC,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ELF32 file with a symbol table of `(name, value, shndx)`.
    fn elf(symbols: &[(&str, u32, u16)]) -> Vec<u8> {
        let mut strtab = vec![0u8];
        let mut symtab = vec![0u8; 16];
        for &(name, value, shndx) in symbols {
            let mut sym = (strtab.len() as u32).to_le_bytes().to_vec();
            strtab.extend(name.as_bytes());
            strtab.push(0);
            sym.extend(value.to_le_bytes());
            sym.extend([0; 6]);
            sym.extend(shndx.to_le_bytes());
            symtab.extend(sym);
        }
        let mut f = vec![0u8; 0x34];
        f[..6].copy_from_slice(b"\x7fELF\x01\x01");
        let symtab_off = f.len();
        f.extend(&symtab);
        let strtab_off = f.len();
        f.extend(&strtab);
        let shoff = f.len();
        f[0x20..0x24].copy_from_slice(&(shoff as u32).to_le_bytes());
        f[0x2e..0x30].copy_from_slice(&40u16.to_le_bytes());
        f[0x30..0x32].copy_from_slice(&3u16.to_le_bytes());
        let header = |kind: u32, off: usize, size: usize, link: u32| {
            let mut h = vec![0u8; 40];
            h[4..8].copy_from_slice(&kind.to_le_bytes());
            h[16..20].copy_from_slice(&(off as u32).to_le_bytes());
            h[20..24].copy_from_slice(&(size as u32).to_le_bytes());
            h[24..28].copy_from_slice(&link.to_le_bytes());
            h
        };
        f.extend(header(0, 0, 0, 0));
        f.extend(header(2, symtab_off, symtab.len(), 2));
        f.extend(header(3, strtab_off, strtab.len(), 0));
        f
    }

    #[test]
    fn reads_absolute_symbols() {
        let f = elf(&[("memcpy", 0x4000_c2c8, 0xfff1), ("main", 0x400d_0020, 1)]);
        let syms = elf_symbols(&f);
        assert_eq!(syms.len(), 3); // with the null symbol
        assert_eq!(
            syms[1],
            ElfSymbol {
                name: "memcpy".into(),
                value: 0x4000_c2c8,
                absolute: true,
                function: false,
            }
        );
        assert!(!syms[2].absolute);
    }

    #[test]
    fn malformed_files_have_no_symbols() {
        assert!(elf_symbols(&[]).is_empty());
        assert!(elf_symbols(b"\x7fELF\x02\x01").is_empty());
        let mut f = elf(&[("memcpy", 0x4000_c2c8, 0xfff1)]);
        f.truncate(f.len() - 20); // cut the section headers short
        let _ = elf_symbols(&f);
    }

    #[test]
    fn rodata_regions() {
        assert!(in_rodata_region(Chip::Esp32, 0x3F40_0120, 0x3F41_8818));
        assert!(!in_rodata_region(Chip::Esp32, 0x3FFB_0000, 0x3FFB_3EA4));
        // The ESP32-C6's flash window holds code and data alike.
        assert!(in_rodata_region(Chip::Esp32C6, 0x4200_0150, 0x4200_0200));
        assert!(in_rodata_region(Chip::Esp32C3, 0x3C00_0020, 0x3C00_0100));
        assert!(!in_rodata_region(Chip::Esp32C3, 0x3FC8_8000, 0x3FC8_8008));
    }

    #[test]
    fn only_flash_contents_become_read_only() {
        assert!(is_flash_contents(".flash.rodata", "PROGBITS"));
        assert!(is_flash_contents(".flash.appdesc", "PROGBITS"));
        // PSRAM in the C5's flash window, and NOLOAD placeholders.
        assert!(!is_flash_contents(".ext_ram.bss", "NOBITS"));
        assert!(!is_flash_contents(".ext_ram_noinit", "NOBITS"));
        assert!(!is_flash_contents(".ext_ram.data", "PROGBITS"));
        assert!(!is_flash_contents(".flash.rodata_noload", "NOBITS"));
    }
}
