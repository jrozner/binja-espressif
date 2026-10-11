//! Types for the mask ROM's functions and data.
//!
//! Each chip's ROM functions get their prototypes, and its ROM variables
//! their types, from a Binary Ninja type library generated offline from
//! ESP-IDF's ROM headers and newlib's headers (scripts/gen_rom_types.py,
//! data/rom_types/<chip>.bntl). The libraries are embedded in the plugin,
//! written to a cache in the user directory at startup (Binary Ninja loads a
//! type library only from a file) and registered for the chip platforms.
//!
//! A type on a symbol at an unmapped address does not reach the calls to it,
//! and Binary Ninja creates functions only where there are bytes. So the
//! ROM's code is mapped, as a memory region of its own filled with zeros
//! (see [`add_rom_region`]), and each typed ROM function is defined there as
//! an imported function with its type. [`RomRecognizer`] makes those
//! functions clobber what real code would, and the Xtensa `WindowRecognizer`
//! leaves their calling convention alone. ROM variables need no region: a
//! data variable at an unmapped address keeps its type.
//!
//! Only names that have a prototype are typed, and only one per address
//! (preferring the name the view already shows), so that the name shown for
//! a ROM function with several names (`ets_printf`, `esp_rom_printf`) stays
//! put. The types are applied when a file is first loaded; the ELF fix-ups
//! apply them again on finalization, which finds them all there, and a
//! database keeps them (see [`apply`]).

use std::collections::{HashMap, HashSet};

use binaryninja::architecture::{
    Architecture, ArchitectureExt, CoreArchitecture, CoreRegister, Register as _, RegisterId,
};
use binaryninja::binary_view::BinaryView;
use binaryninja::calling_convention::CallingConvention;
use binaryninja::confidence::MAX_CONFIDENCE;
use binaryninja::data_buffer::DataBuffer;
use binaryninja::function::Function;
use binaryninja::function_recognizer::FunctionRecognizer;
use binaryninja::low_level_il::LowLevelILRegularFunction;
use binaryninja::platform::Platform;
use binaryninja::rc::Ref;
use binaryninja::segment::SegmentFlags;
use binaryninja::symbol::{SymbolBuilder, SymbolType};
use binaryninja::types::{TypeClass, TypeLibrary};

use crate::espressif::{Chip, cache};

static ESP32: &[u8] = include_bytes!("../../data/rom_types/esp32.bntl");
static ESP32S2: &[u8] = include_bytes!("../../data/rom_types/esp32s2.bntl");
static ESP32S3: &[u8] = include_bytes!("../../data/rom_types/esp32s3.bntl");
static ESP32C2: &[u8] = include_bytes!("../../data/rom_types/esp32c2.bntl");
static ESP32C3: &[u8] = include_bytes!("../../data/rom_types/esp32c3.bntl");
static ESP32C6: &[u8] = include_bytes!("../../data/rom_types/esp32c6.bntl");
static ESP32H2: &[u8] = include_bytes!("../../data/rom_types/esp32h2.bntl");
static ESP32C5: &[u8] = include_bytes!("../../data/rom_types/esp32c5.bntl");

/// The prefix of the names of the memory regions that map the ROM's code.
const REGION_PREFIX: &str = "espressif.rom";

impl Chip {
    /// The chip's ROM type library, as a .bntl file; the ESP8266 has none.
    pub fn rom_type_library(self) -> Option<&'static [u8]> {
        match self {
            Chip::Esp8266 => None,
            Chip::Esp32 => Some(ESP32),
            Chip::Esp32S2 => Some(ESP32S2),
            Chip::Esp32S3 => Some(ESP32S3),
            Chip::Esp32C2 => Some(ESP32C2),
            Chip::Esp32C3 => Some(ESP32C3),
            Chip::Esp32C6 => Some(ESP32C6),
            Chip::Esp32H2 => Some(ESP32H2),
            Chip::Esp32C5 => Some(ESP32C5),
        }
    }

    /// The name of the chip's ROM type library (as gen_rom_types.py names it).
    pub fn rom_type_library_name(self) -> String {
        format!("espressif-rom-{}", self.platform_name())
    }
}

/// The name of the memory region that maps the `index`th range of
/// [`Chip::rom_code_ranges`].
pub fn region_name(index: usize) -> String {
    match index {
        0 => REGION_PREFIX.to_string(),
        n => format!("{REGION_PREFIX}.{n}"),
    }
}

/// Is a memory region of that name one of the ROM code regions this module
/// adds?
pub fn is_rom_region_name(name: &str) -> bool {
    name == REGION_PREFIX
        || name
            .strip_prefix(REGION_PREFIX)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Is `addr` in a ROM code region of the view? Their functions are typed
/// from the ROM type library and have no real code to analyze.
pub fn in_rom_region(view: &BinaryView, addr: u64) -> bool {
    view.memory_map()
        .get_active_region_at(addr)
        .is_some_and(|r| is_rom_region_name(&r.name))
}

/// Writes the ROM type libraries to the cache and registers them for the
/// chip platforms, so that views (and reopened databases, which name the
/// libraries their types came from) find them.
pub fn register_libraries() {
    for chip in Chip::ALL {
        library(chip);
    }
}

/// The chip's ROM type library, loading and registering it on first use.
fn library(chip: Chip) -> Option<Ref<TypeLibrary>> {
    let embedded = chip.rom_type_library()?;
    let platform = Platform::by_name(chip.platform_name())?;
    let name = chip.rom_type_library_name();
    if let Some(lib) = TypeLibrary::from_name(platform.arch(), &name) {
        return Some(lib);
    }
    let path = match cache::extract("rom_types", chip.platform_name(), "bntl", embedded, || {
        Ok(embedded.to_vec())
    }) {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(
                "could not write the {} ROM type library to {}: {err}",
                chip.display_name(),
                cache::dir("rom_types").display()
            );
            return None;
        }
    };
    let Some(lib) = TypeLibrary::load_from_file(&path) else {
        tracing::warn!("could not load the ROM type library {}", path.display());
        return None;
    };
    lib.add_platform(&platform);
    lib.register();
    Some(lib)
}

/// The bytes of the ROM's code at `start..start + len`: zeros, as the ROM
/// itself is not bundled; its functions only need their types. (Espressif
/// publishes the ROM images, in esp-rom-elfs, should they ever be bundled.)
fn rom_contents(_chip: Chip, _start: u32, len: usize) -> Vec<u8> {
    vec![0; len]
}

/// Maps the chip's ROM code, as memory regions of its own, unless the view
/// already maps it (a reopened database keeps its regions). Returns whether
/// the ROM code is mapped by these regions.
pub fn add_rom_region(view: &BinaryView, chip: Chip) -> bool {
    let mut map = view.memory_map();
    let mut mapped = true;
    for (i, (start, end)) in chip.rom_code_ranges().into_iter().enumerate() {
        let name = region_name(i);
        if map.get_region(&name).is_some() {
            continue;
        }
        let range = start as u64..end as u64;
        let overlaps = |r: std::ops::Range<u64>| r.start < range.end && range.start < r.end;
        // A memory region can supply bytes without a segment, so both are
        // checked; the plugin's own ROM regions are not in the way.
        let in_map = map
            .regions()
            .into_iter()
            .any(|r| r.enabled && !is_rom_region_name(&r.name) && overlaps(r.start..r.end()));
        if in_map || view.segments().iter().any(|s| overlaps(s.address_range())) {
            // Something else maps (some of) the ROM: a ROM dump, say. Zeros
            // must not hide it.
            tracing::info!(
                "the {} ROM at {start:#x}..{end:#x} is already mapped in part",
                chip.display_name()
            );
            mapped = false;
            continue;
        }
        let contents = DataBuffer::new(&rom_contents(chip, start, (end - start) as usize));
        let flags = SegmentFlags::new()
            .readable(true)
            .executable(true)
            .contains_code(true)
            .deny_write(true);
        if !map.add_data_memory_region(&name, start as u64, &contents, Some(flags)) {
            tracing::warn!(
                "could not map the {} ROM at {start:#x}",
                chip.display_name()
            );
            mapped = false;
        }
    }
    mapped
}

/// Fixes up the functions in the ROM code regions after lifting: they
/// clobber the caller-saved registers of their calling convention, as real
/// code would, where the analysis of their empty bodies finds that they
/// clobber none.
pub struct RomRecognizer;

impl FunctionRecognizer for RomRecognizer {
    fn recognize_low_level_il(
        &self,
        bv: &BinaryView,
        func: &Function,
        _llil: &LowLevelILRegularFunction,
    ) -> bool {
        if in_rom_region(bv, func.start()) {
            clobber_caller_saved(func);
        }
        false
    }
}

fn clobber_caller_saved(func: &Function) {
    let Some(cc) = func.calling_convention() else {
        return;
    };
    let mut wanted: Vec<u32> = cc
        .contents
        .caller_saved_registers()
        .into_iter()
        .map(|id| id.0)
        .collect();
    wanted.sort();
    let current = func.clobbered_registers();
    let mut ids: Vec<u32> = current.contents.iter().map(|r| r.id().0).collect();
    ids.sort();
    if current.confidence == MAX_CONFIDENCE && ids == wanted {
        return;
    }
    let arch = func.arch();
    let regs: Vec<CoreRegister> = wanted
        .into_iter()
        .filter_map(|id| arch.register_from_id(RegisterId(id)))
        .collect();
    func.set_auto_clobbered_registers(regs, MAX_CONFIDENCE);
}

/// Registers [`RomRecognizer`] for the architectures of the chips that have
/// ROM type libraries.
pub fn register_recognizer() {
    let mut archs = Vec::new();
    for chip in Chip::ALL
        .into_iter()
        .filter(|c| c.rom_type_library().is_some())
    {
        let name = chip.arch_name();
        if archs.contains(&name) {
            continue;
        }
        archs.push(name);
        if let Some(arch) = CoreArchitecture::by_name(name) {
            arch.register_function_recognizer(RomRecognizer);
        }
    }
}

/// From ROM symbols, in order of preference, the ones to type: one per
/// address, with a type. A name `shown` gives for an address (the one the
/// view shows) wins over the order, if it has a type.
pub fn pick_typed<'a>(
    candidates: impl IntoIterator<Item = (&'a str, u32)>,
    has_type: impl Fn(&str) -> bool,
    shown: impl Fn(u32) -> Option<String>,
) -> Vec<(String, u32)> {
    let mut picked: Vec<(String, u32)> = Vec::new();
    let mut seen = HashSet::new();
    for (name, addr) in candidates {
        if !seen.insert(addr) {
            continue;
        }
        let shown = shown(addr).filter(|s| has_type(s));
        if let Some(name) = shown.or_else(|| has_type(name).then(|| name.to_string())) {
            picked.push((name, addr));
        } else {
            // Another name at this address may still have a type.
            seen.remove(&addr);
        }
    }
    picked
}

/// Types the ROM functions and variables among `symbols` (name and address,
/// in order of preference) in the view, from the chip's ROM type library.
pub fn apply<'a>(view: &BinaryView, chip: Chip, symbols: impl IntoIterator<Item = (&'a str, u32)>) {
    // A database keeps the region, functions and variables, and restores
    // its memory map only after the view is set up (the ELF fix-ups run
    // again then): a region added now would be added twice, and the memory
    // map change makes the analysis differ from the saved one.
    if view.file().is_database_backed() {
        return;
    }
    let Some(platform) = view.default_platform() else {
        return;
    };
    let Some(lib) = library(chip) else {
        return;
    };
    let objects: HashMap<String, TypeClass> = lib
        .named_objects()
        .iter()
        .map(|o| (o.name.to_string(), o.ty.type_class()))
        .collect();
    let (functions, data): (Vec<_>, Vec<_>) = symbols
        .into_iter()
        .partition(|&(_, addr)| chip.is_rom_function(addr));
    let shown = |addr: u32| {
        view.symbol_by_address(addr as u64)
            .map(|s| s.raw_name().to_string_lossy().into_owned())
    };
    let functions = pick_typed(
        functions,
        |name| objects.get(name) == Some(&TypeClass::FunctionTypeClass),
        shown,
    );
    let data = pick_typed(
        data,
        |name| {
            objects
                .get(name)
                .is_some_and(|&c| c != TypeClass::FunctionTypeClass)
        },
        shown,
    );
    if functions.is_empty() && data.is_empty() {
        return;
    }
    if view.type_library_by_name(&lib.name()).is_none() {
        view.add_type_library(&lib);
    }

    let mut typed = 0;
    if !functions.is_empty() && add_rom_region(view, chip) {
        for (name, addr) in &functions {
            let addr = *addr as u64;
            if view.function_at(&platform, addr).is_some() {
                continue; // typed before, or by the user
            }
            let Some(ty) = view.import_type_library_object(name.as_str(), Some(&lib)) else {
                continue;
            };
            let sym =
                SymbolBuilder::new(SymbolType::ImportedFunction, name.as_str(), addr).create();
            view.define_auto_symbol_with_type(&sym, &platform, &*ty);
            view.record_imported_object_library(&lib, name.as_str(), addr, &platform);
            typed += 1;
        }
    }
    for (name, addr) in &data {
        let addr = *addr as u64;
        if view.data_variable_at_address(addr).is_some() {
            continue;
        }
        let Some(ty) = view.import_type_library_object(name.as_str(), Some(&lib)) else {
            continue;
        };
        view.define_auto_data_var(addr, &*ty);
        view.record_imported_object_library(&lib, name.as_str(), addr, &platform);
        typed += 1;
    }
    if typed > 0 {
        tracing::info!("typed {typed} mask ROM functions and variables");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chip_but_the_esp8266_has_a_library() {
        for chip in Chip::ALL {
            let lib = chip.rom_type_library();
            assert_eq!(lib.is_none(), chip == Chip::Esp8266, "{chip:?}");
            if let Some(lib) = lib {
                assert!(lib.starts_with(b"BNT"), "{chip:?} is not a type library");
            }
        }
    }

    #[test]
    fn region_names() {
        assert_eq!(region_name(0), "espressif.rom");
        assert_eq!(region_name(2), "espressif.rom.2");
        for i in 0..3 {
            assert!(is_rom_region_name(&region_name(i)));
        }
        for name in [
            "",
            "espressif",
            "espressif.rom.",
            "espressif.rom.x",
            "espressif.romx",
            "rom",
        ] {
            assert!(!is_rom_region_name(name), "{name}");
        }
    }

    fn typed<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |n| names.contains(&n)
    }

    #[test]
    fn one_typed_name_per_address() {
        let candidates = [
            ("ets_printf", 0x40007d54),
            ("esp_rom_printf", 0x40007d54),
            ("untyped", 0x40001000),
            ("SPIRead", 0x40062ed8),
            ("esp_rom_spiflash_read", 0x40062ed8),
        ];
        let picked = pick_typed(
            candidates,
            typed(&["ets_printf", "esp_rom_printf", "esp_rom_spiflash_read"]),
            |_| None,
        );
        assert_eq!(
            picked,
            [
                ("ets_printf".to_string(), 0x40007d54),
                ("esp_rom_spiflash_read".to_string(), 0x40062ed8)
            ]
        );
    }

    #[test]
    fn the_shown_name_wins_if_it_is_typed() {
        let candidates = [("ets_printf", 0x40007d54), ("esp_rom_printf", 0x40007d54)];
        let has = typed(&["ets_printf", "esp_rom_printf"]);
        let shown = |_| Some("esp_rom_printf".to_string());
        assert_eq!(
            pick_typed(candidates, &has, shown),
            [("esp_rom_printf".to_string(), 0x40007d54)]
        );
        // An untyped name shown at the address does not count.
        let shown = |_| Some("printf_alias".to_string());
        assert_eq!(
            pick_typed(candidates, &has, shown),
            [("ets_printf".to_string(), 0x40007d54)]
        );
    }

    #[test]
    fn revision_specific_functions_follow_the_image() {
        // The ESP32 v3.0 ROM's Secure Boot v2 API, in the generated library,
        // is only typed for images that require v3.0.
        let names = |rev| -> Vec<&str> {
            Chip::Esp32
                .rom_symbols_for_revision(rev)
                .map(|&(n, _)| n)
                .collect()
        };
        let has = typed(&["ets_use_secure_boot_v2", "memset"]);
        let pick = |rev| {
            let syms: Vec<(&str, u32)> =
                Chip::Esp32.rom_symbols_for_revision(rev).copied().collect();
            pick_typed(syms, &has, |_| None)
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
        };
        assert!(!names(0).contains(&"ets_use_secure_boot_v2"));
        assert!(names(300).contains(&"ets_use_secure_boot_v2"));
        assert_eq!(pick(0), ["memset"]);
        assert_eq!(pick(300), ["memset", "ets_use_secure_boot_v2"]);
    }
}
