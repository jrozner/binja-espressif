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
//! imported function with its type, recorded as imported from the library
//! (`record_imported_object_library`). [`RomRecognizer`] makes those
//! functions clobber what the API's callers may assume, and the Xtensa
//! architecture leaves the calling convention and clobbers of a function
//! whose type was imported alone. ROM variables
//! need no region: a data variable at an unmapped address keeps its type.
//!
//! The region holds the chip's ROM image when one is available (see
//! `rom_images` and `rom_download`), and zeros otherwise. With an image, the
//! ROM's read-only data is mapped too, where the data bus sees it (see
//! [`add_rom_data_region`]), so the variables there have values. The
//! typed functions have real bodies, which Binary Ninja analyzes: their
//! names and prototypes are then pinned against what that analysis would
//! change, the functions behind the ROM's jump table are typed too, and the
//! ROM's other functions get the image's names, and become functions
//! unless `espressif.rom.createFunctions` is off (see [`use_image`]); then
//! analysis creates functions in the ROM only where code calls them. The
//! region is never swept: the image's symbol table says where its
//! functions are.
//!
//! Only names that have a prototype are typed, and only one per address
//! (preferring the name the view already shows), so that the name shown for
//! a ROM function with several names (`ets_printf`, `esp_rom_printf`) stays
//! put. The types are applied when a file is first loaded; the ELF fix-ups
//! apply them again on finalization, which finds them all there, and a
//! database keeps them (see [`apply`]).
//!
//! A database keeps the types it was saved with, and only names the library
//! (by name and GUID): reopened with a plugin whose library differs, it
//! would keep the earlier prototypes and types and leave the ROM functions
//! the library newly types untyped. [`update_database`] brings it up to
//! date, once per library.

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
use sha2::{Digest, Sha256};

use crate::image::RegionKind;
use crate::rom_images::{Found, RomElf};
use crate::{Chip, cache, rom_download, rom_status};

static ESP32: &[u8] = include_bytes!("../data/rom_types/esp32.bntl");
static ESP32S2: &[u8] = include_bytes!("../data/rom_types/esp32s2.bntl");
static ESP32S3: &[u8] = include_bytes!("../data/rom_types/esp32s3.bntl");
static ESP32C2: &[u8] = include_bytes!("../data/rom_types/esp32c2.bntl");
static ESP32C3: &[u8] = include_bytes!("../data/rom_types/esp32c3.bntl");
static ESP32C6: &[u8] = include_bytes!("../data/rom_types/esp32c6.bntl");
static ESP32H2: &[u8] = include_bytes!("../data/rom_types/esp32h2.bntl");
static ESP32C5: &[u8] = include_bytes!("../data/rom_types/esp32c5.bntl");

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

/// The view metadata that names the ROM type library contents (see
/// [`library_version`]) the view's ROM functions and variables were typed
/// from.
const TYPES_METADATA: &str = "espressif.rom.typeLibrary";

/// What identifies the contents of the chip's ROM type library: a digest of
/// the embedded file. gen_rom_types.py does not write the same bytes twice,
/// so a regenerated library always counts as a new one, even if its
/// contents are the same; [`update_database`] then has nothing to change.
fn library_version(chip: Chip) -> Option<String> {
    let digest = Sha256::digest(chip.rom_type_library()?);
    Some(digest[..16].iter().map(|b| format!("{b:02x}")).collect())
}

/// Records that the view is typed from the chip's ROM type library as
/// embedded in this build.
fn record_library_version(view: &BinaryView, chip: Chip) {
    if let Some(version) = library_version(chip) {
        view.store_metadata(
            TYPES_METADATA,
            version.as_str(),
            MetadataStoreFlags::PERSISTENT,
        );
    }
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
/// hold the chip's ROM image if one is available, or headless, downloaded
/// now (see [`rom_download::rom_image_for_load`]), else zeros. Returns
/// whether the ROM code is
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
            image = rom_download::rom_image_for_load(view, chip);
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
    match &image {
        Some(found) => {
            // The data alias first: the image's status says whether it is
            // mapped.
            add_rom_data_region(view, chip, &found.elf);
            record_image(view, chip, found);
        }
        None => rom_status::record_unmapped(view, chip),
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
    rom_status::record_mapped(view, chip, found);
    tracing::info!(
        "mapped the {} ROM v{}.{} from {}",
        chip.display_name(),
        found.revision / 100,
        found.revision % 100,
        found.path.display()
    );
}

/// The name of the ROM image the view's ROM code regions hold, if any.
pub fn held_image(view: &BinaryView) -> Option<String> {
    view.get_metadata::<String>(IMAGE_METADATA)
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
        add_rom_data_region(view, chip, &found.elf);
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
///   gives them, where the view has none, and become functions, unless
///   [`rom_download::CREATE_FUNCTIONS`] is off: then only analysis creates
///   functions, where code calls them.
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
        if pin_rom_function(view, &lib, func, name, name, false) {
            pinned += 1;
        }
        if pin_implementation(view, &lib, &platform, elf, func.start(), name, false) {
            pinned += 1;
        }
    }
    let named = name_image_functions(view, elf);
    let created = if rom_download::create_functions(view) {
        create_image_functions(view, elf)
    } else {
        0
    };
    tracing::info!(
        "pinned {pinned} typed {} ROM functions, named {named} others and created {created} functions from the ROM image",
        chip.display_name()
    );
}

/// Gives the function a ROM function at `addr` is implemented by, if it is
/// a jump table entry (see [`RomElf::implementation`]), the prototype
/// `name` and its own name, pinned (see [`pin_rom_function`]), unless it
/// has a type the user gave it and not `force`. Returns whether it pinned
/// them.
fn pin_implementation(
    view: &BinaryView,
    lib: &TypeLibrary,
    platform: &Platform,
    elf: &RomElf,
    addr: u64,
    name: &str,
    force: bool,
) -> bool {
    let Some((imp_name, imp)) = elf.implementation(addr as u32) else {
        return false;
    };
    let imp = imp as u64;
    if !force
        && view
            .function_at(platform, imp)
            .is_some_and(|f| f.has_user_type())
    {
        return false;
    }
    let Some(ty) = view.import_type_library_object(name, Some(lib)) else {
        return false;
    };
    let sym = SymbolBuilder::new(SymbolType::Function, imp_name, imp).create();
    view.define_auto_symbol_with_type(&sym, platform, &*ty);
    view.function_at(platform, imp)
        .is_some_and(|f| pin_rom_function(view, lib, &f, name, imp_name, force))
}

/// Names each function the ROM image's symbol table names, where the view
/// has no symbol, and returns how many it named.
pub fn name_image_functions(view: &BinaryView, elf: &RomElf) -> usize {
    let mut named = 0;
    for (name, addr) in elf.functions() {
        let addr = *addr as u64;
        if view.symbol_by_address(addr).is_none() {
            let sym = SymbolBuilder::new(SymbolType::Function, name.as_str(), addr).create();
            view.define_auto_symbol(&sym);
            named += 1;
        }
    }
    named
}

/// Creates a function at each function the ROM image's symbol table names
/// where the view has none, and returns how many it created. They are not
/// auto-discovered: the symbol table says they are functions, so analysis
/// keeps them whether or not anything calls them.
pub fn create_image_functions(view: &BinaryView, elf: &RomElf) -> usize {
    let Some(platform) = view.default_platform() else {
        return 0;
    };
    elf.functions()
        .iter()
        .map(|&(_, addr)| addr as u64)
        .filter(|&addr| {
            view.function_at(&platform, addr).is_none()
                && view
                    .add_auto_function_ext(addr, &platform, None, false)
                    .is_some()
        })
        .count()
}

/// The name of the memory region that maps the ROM's data bus alias (not a
/// ROM code region: see [`is_rom_region_name`]).
pub const DATA_REGION: &str = "espressif.rom.data";

/// Maps the ROM's read-only data where the data bus sees it (the chip's
/// [`RegionKind::RomData`] region, from 0x3FF0_0000 or so), as the ROM
/// image `elf` loads it there, unless the view maps it already. Returns
/// whether it mapped it.
///
/// The ROM's code reads its constants (`_ctype_`, the ROM's strings, the
/// tables of its drivers) there and not where the ROM holds them: with the
/// image's bytes there, those reads and the variables typed there (see
/// [`apply`]) have their values, and references into them resolve. Only
/// with an image: zeros would read as the constants' values. The region is
/// read-only data, and spans the image's data there, not the whole alias.
///
/// [`RegionKind::RomData`]: crate::image::RegionKind::RomData
pub fn add_rom_data_region(view: &BinaryView, chip: Chip, elf: &RomElf) -> bool {
    let mut map = view.memory_map();
    if map.get_region(DATA_REGION).is_some() {
        return false;
    }
    let Some((start, data)) = chip
        .regions()
        .iter()
        .filter(|r| r.kind == RegionKind::RomData)
        .find_map(|r| elf.data_alias(r.start, r.end))
    else {
        return false;
    };
    let range = start as u64..start as u64 + data.len() as u64;
    let overlaps = |r: std::ops::Range<u64>| r.start < range.end && range.start < r.end;
    if map
        .regions()
        .into_iter()
        .any(|r| r.enabled && overlaps(r.start..r.end()))
        || view.segments().iter().any(|s| overlaps(s.address_range()))
    {
        tracing::info!(
            "the {} ROM's data at {:#x}..{:#x} is already mapped in part",
            chip.display_name(),
            range.start,
            range.end
        );
        return false;
    }
    let flags = SegmentFlags::new()
        .readable(true)
        .contains_data(true)
        .deny_write(true)
        .deny_execute(true);
    let added = map.add_data_memory_region(
        DATA_REGION,
        range.start,
        &DataBuffer::new(&data),
        Some(flags),
    );
    if added {
        tracing::info!(
            "mapped the {} ROM's data at {:#x}..{:#x}",
            chip.display_name(),
            range.start,
            range.end
        );
    } else {
        tracing::warn!(
            "could not map the {} ROM's data at {start:#x}",
            chip.display_name()
        );
    }
    added
}

/// Do the view's ROM code regions hold the ROM image `elf`, byte for byte?
pub fn holds_image(view: &BinaryView, chip: Chip, elf: &RomElf) -> bool {
    if view.query_metadata(IMAGE_METADATA).is_none() {
        return false;
    }
    chip.rom_code_ranges().into_iter().all(|(start, end)| {
        let len = (end - start) as usize;
        view.read_vec(start as u64, len) == elf.contents(start, len)
    })
}

/// The name of the typed ROM function at the start of `func`: a ROM symbol
/// there (an imported function, see [`apply`]) that has a prototype in the
/// chip's ROM type library, in a ROM code region. Functions in the region
/// that are not (the ones analysis finds in a ROM image) are ordinary code.
///
/// In the chip's ROM ELF file opened as the view (see [`type_rom_elf`]),
/// whose segments hold the ROM, it is the name of a function in the ROM's
/// code whose type is the user's (`type_rom_elf` pins the prototypes) and
/// whose symbol has a prototype in the library.
///
/// It only reads the view: recognizers call it.
pub fn rom_function_name(view: &BinaryView, func: &Function) -> Option<String> {
    let addr = func.start();
    let chip = Chip::from_platform_name(&func.platform().name())?;
    let kinds: &[SymbolType] = if in_rom_region(view, addr) {
        &[SymbolType::ImportedFunction]
    } else if func.has_user_type()
        && chip
            .rom_code_ranges()
            .iter()
            .any(|&(s, e)| s as u64 <= addr && addr < e as u64)
        && is_own_rom_elf(view, chip)
    {
        &[SymbolType::Function, SymbolType::ImportedFunction]
    } else {
        return None;
    };
    let lib = library(chip)?;
    view.symbols_in_range(addr..addr + 1)
        .iter()
        .filter(|s| s.address() == addr && kinds.contains(&s.sym_type()))
        .map(|s| s.raw_name().to_string_lossy().into_owned())
        .find(|name| {
            lib.get_named_object(name.as_str().into())
                .is_some_and(|t| t.type_class() == TypeClass::FunctionTypeClass)
        })
}

/// The view metadata that marks a view as the chip's ROM ELF file (the
/// chip's platform name), whose typed ROM functions [`type_rom_elf`] pinned.
const ROM_ELF_METADATA: &str = "espressif.rom.elf";

fn is_own_rom_elf(view: &BinaryView, chip: Chip) -> bool {
    view.get_metadata::<String>(ROM_ELF_METADATA)
        .is_some_and(|name| name == chip.platform_name())
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
/// kind is kept. A type the user gave the function is kept as it is, unless
/// `force` (see [`update_database`]). Returns whether it pinned them.
fn pin_rom_function(
    view: &BinaryView,
    lib: &TypeLibrary,
    func: &Function,
    name: &str,
    symbol: &str,
    force: bool,
) -> bool {
    let Some(ty) = view.import_type_library_object(name, Some(lib)) else {
        return false;
    };
    // A type the user gave the function stays, with all it says (its
    // calling convention, say), and nothing is pinned unless it has the
    // library's prototype. (A reopened database reports every function's
    // type as the user's.)
    let user = (func.has_user_type() && !force).then(|| func.function_type());
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

/// Types the ROM functions of the chip's ROM ELF file itself, `elf`, opened
/// as the view. Its own segments hold the ROM, so no ROM region is added
/// (see [`add_rom_region`]) and [`apply`] types none of them. Each function
/// the ROM table names where the file names it too (or its jump table
/// entry, `__call_<name>`) gets its prototype, pinned as with a ROM image
/// (see [`pin_rom_function`]), and so does the function a jump table entry
/// jumps to. Each is recorded as imported from the ROM type library, which
/// tells the Xtensa architecture to leave its convention and clobbers alone.
pub fn type_rom_elf(view: &BinaryView, chip: Chip, elf: &RomElf) {
    // A database keeps them (see backfill_rom_elf_imports and
    // update_database).
    if view.file().is_database_backed() {
        return;
    }
    let (Some(platform), Some(lib)) = (view.default_platform(), library(chip)) else {
        return;
    };
    pin_rom_elf_functions(view, chip, elf, &lib, &platform, false);
}

/// [`type_rom_elf`]'s work, which [`update_database`] does again with
/// `force` (see [`pin_rom_function`]). Returns how many functions it pinned.
fn pin_rom_elf_functions(
    view: &BinaryView,
    chip: Chip,
    elf: &RomElf,
    lib: &TypeLibrary,
    platform: &Platform,
    force: bool,
) -> usize {
    // Before any function is analyzed: the recognizers then treat the
    // pinned functions as the ROM API they are (see rom_function_name).
    if !is_own_rom_elf(view, chip) {
        view.store_metadata(
            ROM_ELF_METADATA,
            chip.platform_name(),
            MetadataStoreFlags::PERSISTENT,
        );
    }
    let names_at = |addr: u32| -> Vec<String> {
        let addr = addr as u64;
        view.symbols_in_range(addr..addr + 1)
            .iter()
            .filter(|s| s.address() == addr)
            .map(|s| s.raw_name().to_string_lossy().into_owned())
            .collect()
    };
    let (table, revisions) = chip.rom_symbols();
    let tables = std::iter::once(table).chain(revisions.iter().map(|&(_, t)| t));
    let in_file = tables.flatten().copied().filter(|&(name, addr)| {
        let jump = format!("__call_{name}");
        chip.is_rom_function(addr) && names_at(addr).iter().any(|n| *n == name || *n == jump)
    });
    let functions = pick_typed(
        in_file,
        |name| {
            lib.get_named_object(name.into())
                .is_some_and(|t| t.type_class() == TypeClass::FunctionTypeClass)
        },
        |addr| names_at(addr).into_iter().next(),
    );
    if functions.is_empty() {
        return 0;
    }
    if view.type_library_by_name(&lib.name()).is_none() {
        view.add_type_library(lib);
    }
    let function_at = |addr: u64| {
        view.function_at(platform, addr)
            .or_else(|| view.add_auto_function_ext(addr, platform, None, false))
    };
    let mut pinned = 0;
    for (name, addr) in &functions {
        let Some(func) = function_at(*addr as u64) else {
            continue;
        };
        if pin_rom_function(view, lib, &func, name, name, force) {
            view.record_imported_object_library(lib, name.as_str(), *addr as u64, platform);
            pinned += 1;
        }
        if let Some((imp_name, imp)) = elf.implementation(*addr)
            && let Some(imp) = function_at(imp as u64)
            && pin_rom_function(view, lib, &imp, name, imp_name, force)
        {
            view.record_imported_object_library(lib, name.as_str(), imp.start(), platform);
            pinned += 1;
        }
    }
    view.store_metadata(
        ROM_ELF_IMPORTS_RECORDED,
        true,
        MetadataStoreFlags::PERSISTENT,
    );
    record_library_version(view, chip);
    tracing::info!(
        "typed {pinned} functions of the {} ROM ELF file",
        chip.display_name()
    );
    pinned
}

/// The view metadata that marks a view of the chip's ROM ELF file (see
/// [`ROM_ELF_METADATA`]) whose typed functions are all recorded as imported
/// from the ROM type library.
const ROM_ELF_IMPORTS_RECORDED: &str = "espressif.rom.elf.importsRecorded";

/// For a database of the chip's ROM ELF file saved by 0.1.0-rc.2 or
/// earlier, which recorded the typed functions as imported from the ROM
/// type library, but not the functions jump table entries jump to: records
/// those too, once. It runs at the start of an analysis update, when the
/// database's metadata, functions and types have been restored.
pub fn backfill_rom_elf_imports(view: &BinaryView, chip: Chip) {
    if !view.file().is_database_backed()
        || !is_own_rom_elf(view, chip)
        || view.query_metadata(ROM_ELF_IMPORTS_RECORDED).is_some()
    {
        return;
    }
    let (Some(platform), Some(lib)) = (view.default_platform(), library(chip)) else {
        return;
    };
    record_typed_rom_elf_functions(view, &lib, &platform);
    view.store_metadata(
        ROM_ELF_IMPORTS_RECORDED,
        true,
        MetadataStoreFlags::PERSISTENT,
    );
}

/// Records each typed function of a ROM ELF file's database (see
/// [`rom_function_name`]) that is not recorded as imported from the ROM type
/// library yet as imported from it, under its own name.
fn record_typed_rom_elf_functions(view: &BinaryView, lib: &TypeLibrary, platform: &Platform) {
    let mut recorded = 0;
    for func in view.functions().iter() {
        if view
            .lookup_imported_object_library(func.start(), platform)
            .is_some()
        {
            continue;
        }
        if let Some(name) = rom_function_name(view, &func) {
            view.record_imported_object_library(lib, name.as_str(), func.start(), platform);
            recorded += 1;
        }
    }
    if recorded > 0 {
        tracing::info!(
            "recorded {recorded} typed ROM functions as imported from the ROM type library"
        );
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
    record_library_version(view, chip);
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

/// Brings a database typed from an earlier ROM type library (one saved by
/// an earlier build of the plugin) up to date with the chip's library, once:
/// it runs when the database is first analyzed with a library it was not
/// typed from (see [`TYPES_METADATA`]), at the start of its first analysis,
/// when its symbols, functions, types and metadata have been restored.
///
/// A database does not store the library, only its name, its GUID and
/// which of its objects and types the view imported. Reopened, the view
/// finds the library registered under that GUID (this build's), but keeps
/// the prototypes and types it imported from the earlier one, and
/// [`apply`] types nothing in a database. So:
///
/// * The library is added to the view if the view does not list it (its
///   GUID changed, say).
/// * The types imported from the library that it now defines differently
///   take its definition. Those imported under an earlier GUID, which the
///   view no longer finds the library of, are imported again under this
///   one; importing them as they are would add duplicates (`__uint32_t_1`).
/// * The ROM table's names the view has no symbol for yet are defined, as
///   the view's loading defines them (an ELF file's fix-ups do not run
///   again for a database).
/// * The ROM functions and variables are typed as [`apply`] types them
///   ([`pick_typed`] over the ROM symbols the view has), in the ROM code
///   regions the database keeps: the ones not typed yet get the library's
///   prototype, and the typed ones whose prototype changed get the new one.
///   Each is recorded as imported from the library, whatever it was
///   recorded as before. A variable the user defined is left alone, unless
///   it is recorded as imported from the library.
/// * With a ROM image in the regions, their prototypes are pinned and the
///   functions the jump table entries among them jump to are typed, as
///   [`use_image`] does. The chip's ROM ELF file is typed again as
///   [`type_rom_elf`] types it.
///
/// The database cannot tell a prototype or type the user edited from the
/// one the library gave it (a reopened database reports every function's
/// type as the user's), so an edited one that the new library changes is
/// replaced, once.
pub fn update_database(view: &BinaryView, chip: Chip) {
    if !view.file().is_database_backed() {
        return;
    }
    let Some(version) = library_version(chip) else {
        return;
    };
    if view.get_metadata::<String>(TYPES_METADATA).as_deref() == Some(version.as_str()) {
        return;
    }
    let (Some(platform), Some(lib)) = (view.default_platform(), library(chip)) else {
        return;
    };
    // Typed at all: a database saved before the ROM types existed has none
    // of these, and is left alone.
    let own_rom_elf = is_own_rom_elf(view, chip);
    let map = view.memory_map();
    let has_region =
        (0..chip.rom_code_ranges().len()).any(|i| map.get_region(&region_name(i)).is_some());
    if !own_rom_elf && !has_region && view.type_library_by_name(&lib.name()).is_none() {
        return;
    }
    if view.type_library_by_name(&lib.name()).is_none() {
        view.add_type_library(&lib);
    }
    let types = update_imported_types(view, &lib);
    let typed = if own_rom_elf {
        crate::elf::own_rom_elf(view, chip).map_or(0, |elf| {
            pin_rom_elf_functions(view, chip, &elf, &lib, &platform, true)
        })
    } else {
        retype(view, chip, &lib, &platform)
    };
    record_library_version(view, chip);
    tracing::info!(
        "updated the database to the {} ROM type library: {types} types and {typed} functions and variables",
        chip.display_name()
    );
}

/// Updates the types the view imported from `lib` (see [`update_database`])
/// and returns how many it changed.
fn update_imported_types(view: &BinaryView, lib: &TypeLibrary) -> usize {
    let guid = lib.guid();
    let mut changed = 0;
    let mut stale = Vec::new();
    for t in view.types().iter() {
        let Some(new) = lib.get_named_type(t.name.clone()) else {
            continue;
        };
        let Some(id) = view.type_id_by_name(t.name.clone()) else {
            continue;
        };
        // A library's types are imported with the ID `<GUID>:["<name>"]`.
        let Some(source) = id.split_once(":[").map(|(g, _)| g) else {
            continue;
        };
        if source == guid {
            if *t.ty != *new {
                view.define_auto_type_with_id(t.name.clone(), &id, &new);
                changed += 1;
            }
        } else if is_guid(source) && view.lookup_imported_type_library(t.name.clone()).is_none() {
            stale.push((t.name.clone(), id));
        }
    }
    // All at once: importing one imports the types it uses.
    for (_, id) in &stale {
        view.undefine_auto_type(id);
    }
    for (name, _) in stale {
        view.import_type_library_type(name, Some(lib));
        changed += 1;
    }
    changed
}

/// Is `s` a GUID (`8-4-4-4-12` hex digits)?
fn is_guid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Types the ROM functions and variables of a database from `lib` (see
/// [`update_database`]) and returns how many it typed or retyped.
fn retype(view: &BinaryView, chip: Chip, lib: &TypeLibrary, platform: &Platform) -> usize {
    let objects: HashMap<String, TypeClass> = lib
        .named_objects()
        .iter()
        .map(|o| (o.name.to_string(), o.ty.type_class()))
        .collect();
    define_missing_rom_symbols(view, chip);
    let symbols = view_rom_symbols(view, chip);
    let (functions, data): (Vec<_>, Vec<_>) = symbols
        .iter()
        .map(|(name, addr)| (name.as_str(), *addr))
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
    let recorded = |addr: u64, name: &str| {
        view.lookup_imported_object_library(addr, platform)
            .is_some_and(|(l, n)| l.guid() == lib.guid() && n.to_string() == name)
    };
    // A ROM image's typed functions have their prototypes pinned.
    let image = held_image(view)
        .and_then(|_| rom_download::rom_image(view, chip))
        .filter(|found| holds_image(view, chip, &found.elf));
    let mut typed = 0;
    for (name, addr) in &functions {
        let addr = *addr as u64;
        if !in_rom_region(view, addr) {
            continue;
        }
        if let Some(found) = &image {
            let func = view
                .function_at(platform, addr)
                .or_else(|| view.add_auto_function_ext(addr, platform, None, false));
            if let Some(func) = func
                && pin_rom_function(view, lib, &func, name, name, true)
            {
                pin_implementation(view, lib, platform, &found.elf, addr, name, true);
                if !recorded(addr, name) {
                    view.record_imported_object_library(lib, name.as_str(), addr, platform);
                }
                typed += 1;
            }
            continue;
        }
        let Some(ty) = view.import_type_library_object(name.as_str(), Some(lib)) else {
            continue;
        };
        match view.function_at(platform, addr) {
            Some(func) => {
                if *func.function_type() != *ty {
                    if func.has_user_type() {
                        func.set_user_type(&ty);
                    } else {
                        func.set_auto_type(&ty);
                    }
                } else if recorded(addr, name) {
                    continue;
                }
            }
            None => {
                let sym =
                    SymbolBuilder::new(SymbolType::ImportedFunction, name.as_str(), addr).create();
                view.define_auto_symbol_with_type(&sym, platform, &*ty);
            }
        }
        view.record_imported_object_library(lib, name.as_str(), addr, platform);
        typed += 1;
    }
    for (name, addr) in &data {
        let addr = *addr as u64;
        let Some(ty) = view.import_type_library_object(name.as_str(), Some(lib)) else {
            continue;
        };
        match view.data_variable_at_address(addr) {
            Some(var) if *var.ty.contents == *ty && recorded(addr, name) => continue,
            // The user's, typed from the library before.
            Some(var) if !var.auto_discovered => {
                let imported = view
                    .lookup_imported_object_library(addr, platform)
                    .is_some_and(|(l, _)| l.name() == lib.name());
                if !imported {
                    continue;
                }
                view.define_user_data_var(addr, &*ty);
            }
            _ => view.define_auto_data_var(addr, &*ty),
        }
        view.record_imported_object_library(lib, name.as_str(), addr, platform);
        typed += 1;
    }
    typed
}

/// Defines the ROM table's names that the view has no symbol for, at
/// addresses without one: an ESP image's revision's (see
/// [`Chip::rom_symbols_for_revision`]), or for an ELF file, every
/// revision's, as the ELF fix-ups define them.
fn define_missing_rom_symbols(view: &BinaryView, chip: Chip) {
    let names: Vec<(&str, u32)> = if view.view_type() == "ELF" {
        chip.rom_symbols().0.to_vec()
    } else {
        chip.rom_symbols_for_revision(rom_download::min_revision(view))
            .copied()
            .collect()
    };
    for (name, addr) in names {
        let addr64 = addr as u64;
        if view.symbol_by_address(addr64).is_some() || view.symbol_by_raw_name(name).is_some() {
            continue;
        }
        let kind = if chip.is_rom_function(addr) {
            SymbolType::ImportedFunction
        } else {
            SymbolType::Data
        };
        view.define_auto_symbol(&SymbolBuilder::new(kind, name, addr64).create());
    }
}

/// The ROM symbols the view has, by preference as [`apply`] takes them:
/// the ROM tables' (of any revision; an image's view only has its
/// revision's), then the view's other imported functions in the ROM's code
/// (the esp_rom API aliases an ELF file names).
fn view_rom_symbols(view: &BinaryView, chip: Chip) -> Vec<(String, u32)> {
    let (table, revisions) = chip.rom_symbols();
    let tables = std::iter::once(table).chain(revisions.iter().map(|&(_, t)| t));
    let mut symbols: Vec<(String, u32)> = tables
        .flatten()
        .filter(|&&(name, addr)| {
            view.symbol_by_raw_name(name)
                .is_some_and(|s| s.address() == addr as u64)
        })
        .map(|&(name, addr)| (name.to_string(), addr))
        .collect();
    for (start, end) in chip.rom_code_ranges() {
        for s in view.symbols_in_range(start as u64..end as u64).iter() {
            if s.sym_type() == SymbolType::ImportedFunction {
                let name = s.raw_name().to_string_lossy().into_owned();
                symbols.push((name, s.address() as u32));
            }
        }
    }
    symbols
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
    fn guids() {
        assert!(is_guid("1853a62d-97e5-58ef-bc7b-9cdd1465f7b2"));
        assert!(!is_guid("SVD"));
        assert!(!is_guid("1853a62d-97e5-58ef-bc7b-9cdd1465f7b"));
        assert!(!is_guid("1853a62d-97e5-58ef-bc7b9cdd1465f7b2"));
        assert!(!is_guid("1853a62g-97e5-58ef-bc7b-9cdd1465f7b2"));
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
