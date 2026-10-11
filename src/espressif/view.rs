//! A binary view for Espressif firmware images and flash dumps.

use binaryninja::Endianness;
use binaryninja::binary_view::{
    BinaryView, BinaryViewBase, BinaryViewType, CustomBinaryView, CustomBinaryViewType,
};
use binaryninja::platform::Platform;
use binaryninja::rc::Ref;
use binaryninja::section::{SectionBuilder, Semantics};
use binaryninja::segment::{SegmentBuilder, SegmentFlags};
use binaryninja::settings::{QueryOptions, Settings};
use binaryninja::string::BnString;
use binaryninja::symbol::{SymbolBuilder, SymbolType};
use binaryninja::types::{MemberAccess, MemberScope, StructureBuilder, Type};
use binaryninjacore_sys::{
    BNFreeSettings, BNGetBinaryViewDefaultLoadSettingsForData, BNSettingsSerializeSchema,
};

use crate::espressif::image::{self, Firmware, RegionKind, Segment};
use crate::espressif::{Chip, rom_types};

/// The load setting that selects the image of a flash dump to load: an app
/// partition's label, [`BOOTLOADER`], or empty for the default.
pub const PARTITION_SETTING: &str = "loader.esp.partition";
pub const BOOTLOADER: &str = "bootloader";

pub struct EspViewType;

impl CustomBinaryViewType for EspViewType {
    type CustomBinaryView = EspView;
    const NAME: &'static str = "ESP";
    const LONG_NAME: &'static str = "Espressif Firmware Image";

    fn create_binary_view(&self, data: &BinaryView) -> Result<EspView, ()> {
        let bytes = data.read_vec(0, data.len() as usize);
        let choice = data
            .load_settings(Self::NAME)
            .filter(|s| s.contains(PARTITION_SETTING))
            .map(|s| {
                s.get_string_with_opts(PARTITION_SETTING, &mut QueryOptions::new_with_view(data))
            })
            .unwrap_or_default();
        let selection = match choice.as_str() {
            "" => image::Selection::Default,
            BOOTLOADER => image::Selection::Bootloader,
            label => image::Selection::Partition(label),
        };
        let Some(firmware) = image::find_firmware(&bytes, selection) else {
            if selection != image::Selection::Default {
                tracing::error!("no loadable image in the selected partition '{choice}'");
            }
            return Err(());
        };
        Ok(EspView { bytes, firmware })
    }

    /// Flash dumps with a partition table offer a choice of the image to
    /// load: an app partition or the bootloader.
    fn load_settings_for_data(&self, data: &BinaryView) -> Option<Ref<Settings>> {
        let bytes = data.read_vec(0, data.len() as usize);
        let (_, parts) = image::partition_table(&bytes)?;
        let apps = image::app_partitions(&parts);
        let mut values = vec![String::new()];
        let mut descriptions = vec!["The factory app, or the first OTA app".to_string()];
        for p in apps {
            values.push(p.label.clone());
            descriptions.push(format!(
                "App partition '{}' at {:#x} ({} KiB)",
                p.label,
                p.offset,
                p.size / 1024
            ));
        }
        values.push(BOOTLOADER.into());
        descriptions.push("The second stage bootloader".into());

        // The core's defaults (platform, image base, ...) plus the choice.
        // The bindings cannot wrap the core's settings instance, so its
        // schema is copied into a new one.
        let view_type = BinaryViewType::by_name(Self::NAME)?;
        let defaults = unsafe {
            let handle = BNGetBinaryViewDefaultLoadSettingsForData(view_type.handle, data.handle);
            if handle.is_null() {
                return None;
            }
            let schema = BNSettingsSerializeSchema(handle);
            BNFreeSettings(handle);
            BnString::into_string(schema)
        };
        // One instance per open file: the core keeps named instances, and
        // it asks for the load settings of a file more than once.
        let id = format!(
            "{}_load_settings_{}",
            Self::NAME,
            data.file().session_id().0
        );
        let settings = Settings::new_with_id(&id);
        settings.deserialize_schema(&defaults);
        let schema = serde_json::json!({
            "title": "Partition",
            "type": "string",
            "default": "",
            "enum": values,
            "enumDescriptions": descriptions,
            "description": "The image of the flash dump to load.",
        });
        settings.register_setting_json(PARTITION_SETTING, &schema.to_string());
        Some(settings)
    }

    fn is_valid_for(&self, data: &BinaryView) -> bool {
        let mut probe = [0u8; 2];
        let header = data.read(&mut probe[..1], 0) == 1
            && matches!(probe[0], image::IMAGE_MAGIC | image::IMAGE_V2_MAGIC);
        let table = image::DUMP_BASES.iter().any(|&base| {
            data.read(&mut probe, (image::PARTITION_TABLE_OFFSET - base) as u64) == 2
                && probe == [0xAA, 0x50]
        });
        let at_1000 = data.read(&mut probe[..1], 0x1000) == 1
            && matches!(probe[0], image::IMAGE_MAGIC | image::IMAGE_V2_MAGIC);
        if !(header || table || at_1000) || data.len() > 0x0400_0000 {
            return false;
        }
        image::find_firmware(
            &data.read_vec(0, data.len() as usize),
            image::Selection::Default,
        )
        .is_some()
    }
}

pub struct EspView {
    bytes: Vec<u8>,
    firmware: Firmware,
}

impl BinaryViewBase for EspView {
    fn entry_point(&self) -> u64 {
        self.firmware.image.entry as u64
    }

    fn default_endianness(&self) -> Endianness {
        Endianness::LittleEndian
    }

    fn address_size(&self) -> usize {
        4
    }
}

fn flags_for(kind: RegionKind) -> SegmentFlags {
    let f = SegmentFlags::new().readable(true);
    match kind {
        // Code regions are mapped read-only even though IRAM is writable, so
        // that `l32r` literals in them are treated as constants.
        RegionKind::Iram | RegionKind::Irom | RegionKind::RtcCode | RegionKind::Rom => {
            f.executable(true).contains_code(true)
        }
        // Read-only, so that the jump tables and constants among the code
        // are read as constants.
        RegionKind::Flash => f.executable(true).contains_code(true).contains_data(true),
        // A segment here may be code or initialized data; it has to stay
        // writable for the data's sake.
        RegionKind::Sram | RegionKind::RtcRam => f
            .writable(true)
            .executable(true)
            .contains_code(true)
            .contains_data(true),
        RegionKind::Drom | RegionKind::RomData => f.contains_data(true),
        RegionKind::Dram | RegionKind::RtcData | RegionKind::ExtRam | RegionKind::Peripheral => {
            f.writable(true).contains_data(true)
        }
    }
}

fn semantics_for(kind: RegionKind) -> Semantics {
    match kind {
        RegionKind::Iram
        | RegionKind::Irom
        | RegionKind::RtcCode
        | RegionKind::Rom
        | RegionKind::Flash => Semantics::ReadOnlyCode,
        RegionKind::Drom | RegionKind::RomData => Semantics::ReadOnlyData,
        RegionKind::Sram | RegionKind::RtcRam => Semantics::DefaultSection,
        _ => Semantics::ReadWriteData,
    }
}

impl EspView {
    fn chip(&self) -> Chip {
        self.firmware.image.chip
    }

    /// Adds the file-backed segments of the image and a section for each.
    fn add_image_segments(&self, view: &BinaryView) -> Vec<(u64, u64)> {
        let chip = self.chip();
        let mut mapped = Vec::new();
        let mut counts = std::collections::HashMap::new();
        for seg in &self.firmware.image.segments {
            let Segment { addr, offset, size } = *seg;
            if addr == 0 || size == 0 {
                continue; // esptool padding
            }
            let Some(region) = image::region_of(chip, addr) else {
                tracing::warn!(
                    "segment at {addr:#x} is outside the {} memory map",
                    chip.display_name()
                );
                continue;
            };
            let (start, end) = (addr as u64, addr as u64 + size as u64);
            if mapped.iter().any(|&(s, e)| start < e && s < end) {
                tracing::warn!("segment at {addr:#x} overlaps another segment; skipped");
                continue;
            }
            view.add_segment(
                SegmentBuilder::new(start..end)
                    .parent_backing(offset as u64..offset as u64 + size as u64)
                    .flags(flags_for(region.kind))
                    .is_auto(true),
            );
            let n = counts.entry(region.name).or_insert(0);
            let name = if *n == 0 {
                region.name.to_string()
            } else {
                format!("{}.{}", region.name, n)
            };
            *n += 1;
            view.add_section(
                SectionBuilder::new(name, start..end)
                    .semantics(semantics_for(region.kind))
                    .is_auto(true),
            );
            mapped.push((start, end));
        }
        mapped
    }

    /// ESP8266 flash dumps without a partition table: the flash cache window
    /// maps the first megabyte of flash at 0x40200000, where NONOS SDK
    /// `irom0` code lives. With a partition table only the app partition is
    /// loaded; its image maps its own flash segments.
    fn add_esp8266_flash(&self, view: &BinaryView, mapped: &mut Vec<(u64, u64)>) {
        if !self.firmware.map_flash_window {
            return;
        }
        let start = image::ESP8266_FLASH_MAP as u64;
        let len = (self.bytes.len() as u64).min(0x10_0000);
        if mapped.iter().any(|&(s, e)| s < start + len && start < e) {
            return;
        }
        view.add_segment(
            SegmentBuilder::new(start..start + len)
                .parent_backing(0..len)
                .flags(flags_for(RegionKind::Irom))
                .is_auto(true),
        );
        view.add_section(
            SectionBuilder::new("flash".into(), start..start + len)
                .semantics(Semantics::ReadOnlyCode)
                .is_auto(true),
        );
        mapped.push((start, start + len));
    }

    /// Maps the parts of the RAM and peripheral regions that the image does
    /// not initialize (`.bss`, heap, memory mapped registers) so that
    /// references to them resolve.
    fn add_uninitialized(&self, view: &BinaryView, mapped: &[(u64, u64)]) {
        for region in self.chip().regions() {
            if !matches!(
                region.kind,
                RegionKind::Dram
                    | RegionKind::Sram
                    | RegionKind::RtcData
                    | RegionKind::RtcRam
                    | RegionKind::Peripheral
            ) {
                continue;
            }
            // What is left of unified SRAM is `.bss`, heap and stack; of RTC
            // memory, the retained `.rtc.bss` and `.rtc_noinit`.
            let kind = match region.kind {
                RegionKind::Sram => RegionKind::Dram,
                RegionKind::RtcRam => RegionKind::RtcData,
                kind => kind,
            };
            let mut cursor = region.start as u64;
            let end = region.end as u64;
            let mut inside: Vec<(u64, u64)> = mapped
                .iter()
                .copied()
                .filter(|&(s, e)| s < end && cursor < e)
                .collect();
            inside.sort();
            let mut gaps = Vec::new();
            for (s, e) in inside {
                if s > cursor {
                    gaps.push((cursor, s));
                }
                cursor = cursor.max(e);
            }
            if cursor < end {
                gaps.push((cursor, end));
            }
            for (s, e) in gaps {
                view.add_segment(
                    SegmentBuilder::new(s..e)
                        .flags(flags_for(kind))
                        .is_auto(true),
                );
                let name = if region.kind == RegionKind::Peripheral {
                    region.name.to_string()
                } else {
                    format!("{}.bss@{s:x}", region.name)
                };
                view.add_section(
                    SectionBuilder::new(name, s..e)
                        .semantics(semantics_for(kind))
                        .is_auto(true),
                );
            }
        }
    }

    /// Seeds functions, and names them where possible, from what the chip's
    /// CPU core finds in the image's code segments (see
    /// [`crate::espressif::Cpu::scan_code`]).
    fn add_scanned_functions(&self, view: &BinaryView, platform: &Platform) {
        let cpu = self.chip().cpu();
        let mut found = 0;
        for seg in &self.firmware.image.segments {
            let Some(region) = image::region_of(self.chip(), seg.addr) else {
                continue;
            };
            if !region.is_code() || seg.addr == 0 {
                continue;
            }
            let data = &self.bytes[seg.offset..seg.offset + seg.size as usize];
            let scan = cpu.scan_code(seg.addr, data);
            for (addr, name) in scan.functions {
                view.add_auto_function_with_platform(addr as u64, platform);
                if let Some(name) = name {
                    let sym = SymbolBuilder::new(SymbolType::Function, name, addr as u64).create();
                    view.define_auto_symbol(&sym);
                }
                found += 1;
            }
            for (addr, name, len) in scan.pointer_tables {
                let code_ptr = Type::pointer(&platform.arch(), &Type::void());
                let sym = SymbolBuilder::new(SymbolType::Data, name, addr as u64).create();
                view.define_auto_symbol_with_type(
                    &sym,
                    platform,
                    &*Type::array(&code_ptr, len as u64),
                );
            }
        }
        if found > 0 {
            tracing::info!("found {found} functions by scanning the code");
        }
    }

    /// Names the global pointer the startup code at the entry point sets,
    /// and gives it to the analysis, so that `gp`-relative accesses to
    /// small data resolve (see [`crate::riscv::set_global_pointer`]).
    fn define_global_pointer(&self, view: &BinaryView) {
        let entry = self.firmware.image.entry;
        let Some(seg) = self
            .firmware
            .image
            .segments
            .iter()
            .find(|s| s.addr <= entry && entry - s.addr < s.size)
        else {
            return;
        };
        let code =
            &self.bytes[seg.offset + (entry - seg.addr) as usize..seg.offset + seg.size as usize];
        let Some(gp) = self.chip().cpu().global_pointer(entry, code) else {
            return;
        };
        tracing::info!("the startup code sets gp to {gp:#x}");
        view.define_auto_symbol(
            &SymbolBuilder::new(SymbolType::Data, "__global_pointer$", gp as u64).create(),
        );
        crate::riscv::set_global_pointer(view, gp);
    }

    /// Names the mask ROM's functions and data (and, on the ESP8266, the
    /// peripheral register blocks; the other chips' peripherals come from
    /// Espressif's SVD files through Binary Ninja's SVD loader). The ROM is
    /// not part of the image, so its functions are defined
    /// as imported functions at their (unmapped) addresses
    /// (see [`Chip::is_rom_function`]).
    ///
    /// Some functions only exist in the ROM of later chip revisions (the
    /// ESP32 v3.0 ROM's Secure Boot v2 API); those are named only when the
    /// image requires such a revision (see [`Chip::rom_symbols_for_revision`]).
    ///
    /// The functions and variables that have types in the chip's ROM type
    /// library are then typed (see [`rom_types`]).
    fn define_rom_symbols(&self, view: &BinaryView) {
        let chip = self.chip();
        let min_rev = self.firmware.image.min_chip_rev;
        for &(name, addr) in chip.rom_symbols_for_revision(min_rev) {
            let kind = if chip.is_rom_function(addr) {
                SymbolType::ImportedFunction
            } else {
                SymbolType::Data
            };
            view.define_auto_symbol(&SymbolBuilder::new(kind, name, addr as u64).create());
        }
        rom_types::apply(view, chip, chip.rom_symbols_for_revision(min_rev).copied());
    }

    fn define_app_desc(&self, view: &BinaryView, platform: &Platform) {
        let Some((addr, desc)) = self.firmware.image.app_desc(&self.bytes) else {
            return;
        };
        let u32_t = Type::int(4, false);
        let u8_t = Type::int(1, false);
        let char_t = Type::char();
        let mut s = StructureBuilder::new();
        let field = |s: &mut StructureBuilder, ty: &Type, name: &str| {
            s.append(ty, name, MemberAccess::NoAccess, MemberScope::NoScope);
        };
        field(&mut s, &u32_t, "magic_word");
        field(&mut s, &u32_t, "secure_version");
        field(&mut s, &Type::array(&u32_t, 2), "reserv1");
        field(&mut s, &Type::array(&char_t, 32), "version");
        field(&mut s, &Type::array(&char_t, 32), "project_name");
        field(&mut s, &Type::array(&char_t, 16), "time");
        field(&mut s, &Type::array(&char_t, 16), "date");
        field(&mut s, &Type::array(&char_t, 32), "idf_ver");
        field(&mut s, &Type::array(&u8_t, 32), "app_elf_sha256");
        field(&mut s, &Type::array(&u32_t, 20), "reserv2");
        let ty = Type::structure(&s.finalize());
        let name = view.define_auto_type("esp_app_desc_t", "esp", &ty);
        let named = Type::named_type_from_type(name, &ty);
        let sym = SymbolBuilder::new(SymbolType::Data, "esp_app_desc", addr as u64).create();
        view.define_auto_symbol_with_type(&sym, platform, &*named);

        let text = |o: usize, n: usize| {
            let b = &desc[o..o + n];
            String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(n)]).into_owned()
        };
        tracing::info!(
            "ESP-IDF application '{}' version '{}', built {} {} with ESP-IDF {}",
            text(48, 32),
            text(16, 32),
            text(96, 16),
            text(80, 16),
            text(112, 32)
        );
    }
}

impl CustomBinaryView for EspView {
    fn initialize(&mut self, view: &BinaryView) -> bool {
        let chip = self.chip();
        let Some(platform) = Platform::by_name(chip.platform_name()) else {
            tracing::error!("platform {} is not registered", chip.platform_name());
            return false;
        };
        let image = &self.firmware.image;
        tracing::info!(
            "{} {:?} image at file offset {:#x}{}, entry {:#x}, minimum chip revision v{}.{}",
            chip.display_name(),
            image.format,
            image.start,
            match &self.firmware.partition {
                Some(p) => format!(" (partition '{}')", p.label),
                None => String::new(),
            },
            image.entry,
            image.min_chip_rev / 100,
            image.min_chip_rev % 100
        );
        view.set_default_platform(&platform);

        let mut mapped = self.add_image_segments(view);
        if mapped.is_empty() {
            return false;
        }
        self.add_esp8266_flash(view, &mut mapped);
        self.add_uninitialized(view, &mapped);

        let entry = image.entry as u64;
        view.define_auto_symbol(
            &SymbolBuilder::new(SymbolType::Function, chip.entry_symbol_name(), entry).create(),
        );
        view.add_entry_point(entry);
        self.define_global_pointer(view);
        self.add_scanned_functions(view, &platform);
        self.define_app_desc(view, &platform);
        self.define_rom_symbols(view);
        true
    }
}
