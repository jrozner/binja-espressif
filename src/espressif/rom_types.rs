//! Types for the mask ROM's functions and data.
//!
//! Each chip's ROM functions get their prototypes, and its ROM variables
//! their types, from a Binary Ninja type library generated offline from
//! ESP-IDF's ROM headers and newlib's headers (scripts/gen_rom_types.py,
//! data/rom_types/<chip>.bntl). The libraries are embedded in the plugin,
//! written to a cache in the user directory at startup (Binary Ninja loads a
//! type library only from a file) and registered; each names its chip's
//! platform, so the platform finds it.
//!
//! A type on a symbol at an unmapped address does not reach the calls to it,
//! and Binary Ninja creates functions only where there are bytes. So the
//! ROM's code is mapped, as a memory region of its own (see
//! [`add_rom_region`]), and each typed ROM function is defined there as an
//! imported function with its type. [`RomRecognizer`] makes those functions
//! clobber what the API's callers may assume, and the Xtensa
//! `WindowRecognizer` leaves their calling convention alone. ROM variables
//! need no region: a data variable at an unmapped address keeps its type.
//!
//! The region holds the chip's ROM image when one is available (see
//! `rom_images` and `rom_download`), and zeros otherwise. With an image, the
//! typed functions have real bodies, which Binary Ninja analyzes: their
//! names and prototypes are then pinned against what that analysis would
//! change, the functions behind the ROM's jump table are typed too, and the
//! ROM's other functions get the image's names (see [`use_image`]).
//! Analysis creates functions in the ROM only where code calls them; the
//! region is not swept.
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
use binaryninja::binary_view::{BinaryView, MetadataStoreFlags};
use binaryninja::calling_convention::CallingConvention;
use binaryninja::confidence::{Conf, MAX_CONFIDENCE};
use binaryninja::data_buffer::DataBuffer;
use binaryninja::function::Function;
use binaryninja::function_recognizer::FunctionRecognizer;
use binaryninja::low_level_il::LowLevelILRegularFunction;
use binaryninja::platform::Platform;
use binaryninja::rc::Ref;
use binaryninja::segment::SegmentFlags;
use binaryninja::symbol::{SymbolBuilder, SymbolType};
use binaryninja::types::{Type, TypeClass, TypeLibrary};

use crate::espressif::rom_images::{Found, RomElf};
use crate::espressif::{Chip, cache, rom_download};

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
    // gen_rom_types.py names the platform in the library, and a library
    // loaded from a file is finalized: adding the platform again here only
    // makes the core log "Attempt to mutate immutable library".
    if !lib
        .platform_names()
        .iter()
        .any(|p| p == chip.platform_name())
    {
        tracing::warn!(
            "the ROM type library {} is not for the {} platform",
            path.display(),
            chip.platform_name()
        );
        return None;
    }
    lib.register();
    Some(lib)
}

/// The view metadata that names the ROM image the ROM code regions hold;
/// without it they hold zeros.
const IMAGE_METADATA: &str = "espressif.rom.image";

/// The bytes of the ROM's code at `start..start + len`: the ROM image's, if
/// there is one (see [`rom_images`](super::rom_images)), else zeros, as the
/// ROM itself is not bundled and its functions only need their types.
fn rom_contents(image: Option<&RomElf>, start: u32, len: usize) -> Vec<u8> {
    match image {
        Some(elf) => elf.contents(start, len),
        None => vec![0; len],
    }
}

fn region_flags() -> SegmentFlags {
    SegmentFlags::new()
        .readable(true)
        .executable(true)
        .contains_code(true)
        .deny_write(true)
}

/// Maps the chip's ROM code, as memory regions of its own, unless the view
/// already maps it (a reopened database keeps its regions). The regions
/// hold the chip's ROM image if one is available (see
/// [`rom_download::rom_image`]), else zeros. Returns whether the ROM code is
/// mapped by these regions, and the ROM image they hold.
pub fn add_rom_region(view: &BinaryView, chip: Chip) -> (bool, Option<Found>) {
    let mut map = view.memory_map();
    let mut mapped = true;
    let mut image = None;
    let mut added = false;
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
        if !added {
            image = rom_download::rom_image(view, chip);
        }
        let elf = image.as_ref().map(|f| &f.elf);
        let contents = DataBuffer::new(&rom_contents(elf, start, (end - start) as usize));
        if map.add_data_memory_region(&name, start as u64, &contents, Some(region_flags())) {
            added = true;
        } else {
            tracing::warn!(
                "could not map the {} ROM at {start:#x}",
                chip.display_name()
            );
            mapped = false;
        }
    }
    if !added {
        return (mapped, None);
    }
    if let Some(found) = &image {
        record_image(view, chip, found);
    }
    (mapped, image)
}

fn record_image(view: &BinaryView, chip: Chip, found: &Found) {
    let name = found
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    view.store_metadata(
        IMAGE_METADATA,
        name.as_str(),
        MetadataStoreFlags::PERSISTENT,
    );
    tracing::info!(
        "mapped the {} ROM v{}.{} from {}",
        chip.display_name(),
        found.revision / 100,
        found.revision % 100,
        found.path.display()
    );
}

/// Does the view have the chip's ROM code regions, filled with zeros for
/// want of a ROM image?
pub fn has_zero_filled_region(view: &BinaryView, chip: Chip) -> bool {
    if view.query_metadata(IMAGE_METADATA).is_some() {
        return false;
    }
    let map = view.memory_map();
    (0..chip.rom_code_ranges().len()).all(|i| map.get_region(&region_name(i)).is_some())
}

/// Replaces the contents of the view's ROM code regions with the ROM image
/// `found`. The functions, symbols and types in them are kept.
pub fn map_rom_image(view: &BinaryView, chip: Chip, found: &Found) -> bool {
    let mut map = view.memory_map();
    let mut ok = true;
    for (i, (start, end)) in chip.rom_code_ranges().into_iter().enumerate() {
        let name = region_name(i);
        if map.get_region(&name).is_some() && !map.remove_memory_region(&name) {
            ok = false;
            continue;
        }
        let contents = DataBuffer::new(&rom_contents(
            Some(&found.elf),
            start,
            (end - start) as usize,
        ));
        if !map.add_data_memory_region(&name, start as u64, &contents, Some(region_flags())) {
            tracing::warn!(
                "could not map the {} ROM at {start:#x}",
                chip.display_name()
            );
            ok = false;
        }
    }
    if ok {
        record_image(view, chip, found);
        use_image(view, chip, &found.elf);
    }
    ok
}

/// What a ROM image changes for the view's ROM functions, once it is
/// mapped:
///
/// * The typed ROM functions' names, prototypes and purity become user
///   data (see [`pin_rom_function`]), so that the analysis of their bodies
///   does not change them.
/// * The functions behind the typed ROM functions that are jump table
///   entries (see [`RomElf::implementation`]) get the same prototype and
///   name, pinned too: a jump table entry is a thunk, whose return value
///   Binary Ninja takes from the function it jumps to.
/// * The ROM's other functions get the names the ELF file's symbol table
///   gives them, where the view has none. This only names them: analysis
///   creates functions where code calls them, not here.
///
/// This runs where nothing else changes the view (the view's
/// initialization, the ELF fix-ups, the image step), not in the analysis of
/// each function, which runs on many threads.
fn use_image(view: &BinaryView, chip: Chip, elf: &RomElf) {
    let (Some(platform), Some(lib)) = (view.default_platform(), library(chip)) else {
        return;
    };
    let typed: Vec<(String, Ref<Function>)> = view
        .functions()
        .iter()
        .filter_map(|f| rom_function_name(view, &f).map(|name| (name, f.to_owned())))
        .collect();
    let mut pinned = 0;
    for (name, func) in &typed {
        if pin_rom_function(view, &lib, func, name, name) {
            pinned += 1;
        }
        let Some((imp_name, imp)) = elf.implementation(func.start() as u32) else {
            continue;
        };
        let imp = imp as u64;
        if view
            .function_at(&platform, imp)
            .is_some_and(|f| f.has_user_type())
        {
            continue;
        }
        let Some(ty) = view.import_type_library_object(name.as_str(), Some(&lib)) else {
            continue;
        };
        let sym = SymbolBuilder::new(SymbolType::Function, imp_name, imp).create();
        view.define_auto_symbol_with_type(&sym, &platform, &*ty);
        if let Some(f) = view.function_at(&platform, imp)
            && pin_rom_function(view, &lib, &f, name, imp_name)
        {
            pinned += 1;
        }
    }
    let mut named = 0;
    for (name, addr) in elf.functions() {
        let addr = *addr as u64;
        if view.symbol_by_address(addr).is_none() {
            let sym = SymbolBuilder::new(SymbolType::Function, name.as_str(), addr).create();
            view.define_auto_symbol(&sym);
            named += 1;
        }
    }
    tracing::info!(
        "pinned {pinned} typed {} ROM functions and named {named} others from the ROM image",
        chip.display_name()
    );
}

/// The name of the typed ROM function at the start of `func`: a ROM symbol
/// there (an imported function, see [`apply`]) that has a prototype in the
/// chip's ROM type library, in a ROM code region. Functions in the region
/// that are not (the ones analysis finds in a ROM image) are ordinary code.
pub fn rom_function_name(view: &BinaryView, func: &Function) -> Option<String> {
    let addr = func.start();
    if !in_rom_region(view, addr) {
        return None;
    }
    let chip = Chip::from_platform_name(&func.platform().name())?;
    let lib = library(chip)?;
    view.symbols_in_range(addr..addr + 1)
        .iter()
        .filter(|s| s.address() == addr && s.sym_type() == SymbolType::ImportedFunction)
        .map(|s| s.raw_name().to_string_lossy().into_owned())
        .find(|name| {
            lib.get_named_object(name.as_str().into())
                .is_some_and(|t| t.type_class() == TypeClass::FunctionTypeClass)
        })
}

/// Fixes up the typed ROM functions (see [`rom_function_name`]) after
/// lifting: they clobber the caller-saved registers of their calling
/// convention. That is the contract of an external API, which callers rely
/// on, where the analysis of a body finds less: nothing at all in a
/// zero-filled region, and in a ROM image possibly too little (a jump table
/// entry's tail jump, an indirect call it cannot follow, code that does not
/// follow the ABI internally).
pub struct RomRecognizer;

impl FunctionRecognizer for RomRecognizer {
    fn recognize_low_level_il(
        &self,
        bv: &BinaryView,
        func: &Function,
        _llil: &LowLevelILRegularFunction,
    ) -> bool {
        if rom_function_name(bv, func).is_some() {
            clobber_caller_saved(func);
        }
        false
    }
}

/// Makes a typed ROM function's name, prototype, purity and return value
/// location user data, from the ROM type library. The ROM header's
/// prototype is what callers follow, but in a ROM image Binary Ninja's
/// analysis of the body would otherwise change them:
///
/// * the jump table entries the RISC-V and ESP32-S3 ROMs export their API
///   through only jump to the implementation, and Binary Ninja names such
///   thunks after their target (`j_sub_40057f76`), over the ROM's name
///   (`memset`), at every call;
/// * it narrows parameter types (`void *` to `uint8_t *`) and changes
///   return values through tail calls;
/// * it infers `__pure` (wrongly, for a jump table entry whose indirect
///   jump it cannot follow), and calls to pure functions whose result is
///   unused vanish from HLIL.
///
/// `name` names the prototype; `symbol` is the function's name, and its
/// kind is kept. A type the user gave the function is kept as it is.
/// Returns whether it pinned them.
fn pin_rom_function(
    view: &BinaryView,
    lib: &TypeLibrary,
    func: &Function,
    name: &str,
    symbol: &str,
) -> bool {
    let Some(ty) = view.import_type_library_object(name, Some(lib)) else {
        return false;
    };
    // A type the user gave the function stays, with all it says (its
    // calling convention, say), and nothing is pinned unless it has the
    // library's prototype. (A database reopened from a view without a ROM
    // image reports the library's type as a user type.)
    let user = func.has_user_type().then(|| func.function_type());
    if user.as_ref().is_some_and(|u| !same_prototype(u, &ty)) {
        return false;
    }
    let kind = func.symbol().sym_type();
    let sym = SymbolBuilder::new(kind, symbol, func.start()).create();
    view.define_user_symbol(&sym);
    let ty = match user {
        Some(user) => user,
        None => {
            func.set_user_type(&ty);
            ty
        }
    };
    func.set_user_pure(Conf::new(ty.pure().contents, MAX_CONFIDENCE));
    func.set_user_is_return_value_default_location(true);
    true
}

/// Do two function types have the same return type, parameter types and
/// variable arguments (whatever their parameters' names)?
fn same_prototype(a: &Type, b: &Type) -> bool {
    let signature = |t: &Type| {
        let params: Vec<String> = t
            .parameters()
            .unwrap_or_default()
            .iter()
            .map(|p| p.ty.contents.to_string())
            .collect();
        let ret = t.return_value().map(|r| r.contents.to_string());
        (ret, params, t.has_variable_arguments().contents)
    };
    signature(a) == signature(b)
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
    let (mapped, image) = if functions.is_empty() {
        (false, None)
    } else {
        add_rom_region(view, chip)
    };
    if mapped {
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
    if let Some(found) = image {
        use_image(view, chip, &found.elf);
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
