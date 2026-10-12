//! The `EspFlash` container transform, which splits a flash dump with a
//! partition table into its second stage bootloader, its partition table and
//! one file per partition, named by the partition's label.
//!
//! In the UI, Binary Ninja's container browser offers every file of the dump.
//! Headless (`bn.load`), only the file the `files.espressif.partition`
//! setting chooses is offered, by default the factory (or first OTA) app, so
//! that the session has a single path and that file is opened (as Binary
//! Ninja's own Universal Mach-O transform does with
//! `files.universal.architecturePreference`). The app and bootloader files are
//! firmware images, which the `ESP` view loads; the others are data.
//!
//! The dump as a whole is still loaded by the `ESP` view when the container
//! is not used (`files.container.mode` `Disabled`, `EspFlash` in
//! `files.container.excludedTransforms`, or `*` as the partition headless).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

use binaryninja::binary_view::{BinaryReader, BinaryView, BinaryViewBase, MetadataStoreFlags};
use binaryninja::data_buffer::DataBuffer;
use binaryninja::metadata::Metadata;
use binaryninja::rc::Ref;
use binaryninja::settings::Settings;
use binaryninja::transform::{
    ContextTransform, CustomTransform, DetectionTransform, TransformContext,
    TransformInputParameters, TransformResult, TransformType,
    register_transform_with_detection_and_context,
};
use sha2::{Digest, Sha256};

use crate::image::{self, DUMP_BASES, PARTITION_TABLE_OFFSET, PARTITION_TABLE_SIZE, Partition};

/// The setting that chooses the file of a flash dump to open headless: a
/// partition's label, [`BOOTLOADER`], [`PARTITION_TABLE`], empty for the
/// default (the factory or first OTA app), or [`ALL`] for no choice.
pub const PARTITION_SETTING: &str = "files.espressif.partition";
pub const BOOTLOADER: &str = "bootloader";
pub const PARTITION_TABLE: &str = "partition-table";
pub const ALL: &str = "*";

/// The metadata key, on the views of a file extracted from a flash dump, of
/// where it came from (see [`entry_metadata`]).
pub const ENTRY_METADATA: &str = "espressif.flash.entry";

/// The flash sector the partition table is in.
const TABLE_SECTOR: usize = 0x1000;

/// A file of a flash dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Where it is in flash, and its size there.
    pub flash_offset: u32,
    pub size: u32,
    /// Its bytes in the dump: `None` if the dump ends before it starts,
    /// shorter than `size` if the dump ends inside it.
    pub range: Option<Range<usize>>,
    /// The partition, for the files of partitions.
    pub partition: Option<Partition>,
}

impl Entry {
    /// What the file is, for the container browser.
    pub fn message(&self) -> String {
        let what = match &self.partition {
            Some(p) => format!("{} partition", partition_type_name(p)),
            None if self.name == BOOTLOADER => "second stage bootloader".into(),
            None => "partition table".into(),
        };
        let at = format!("{what} at {:#x}, {:#x} bytes", self.flash_offset, self.size);
        match &self.range {
            None => format!("{at}: not in the dump"),
            Some(r) if r.len() < self.size as usize => {
                format!("{at}: the dump ends after {:#x} bytes", r.len())
            }
            Some(_) => at,
        }
    }

    /// The metadata of the file's context and view.
    fn metadata(&self) -> Vec<(&'static str, Ref<Metadata>)> {
        let mut md = vec![
            ("flashOffset", (self.flash_offset as u64).into()),
            ("size", (self.size as u64).into()),
        ];
        if let Some(p) = &self.partition {
            md.push(("label", p.label.as_str().into()));
            md.push(("type", (p.kind as u64).into()));
            md.push(("subtype", (p.subtype as u64).into()));
        }
        md
    }

    fn bytes<'a>(&self, data: &'a [u8]) -> Option<&'a [u8]> {
        data.get(self.range.clone()?)
    }

    /// Does the file hold a firmware image the `ESP` view loads?
    fn has_image(&self, data: &[u8]) -> bool {
        self.bytes(data)
            .is_some_and(|b| image::parse_image(b, 0).is_some())
    }
}

/// `app/factory`, `data/nvs`, ...: a partition's type and subtype as
/// `gen_esp32part.py` names them.
pub fn partition_type_name(p: &Partition) -> String {
    let kind = match p.kind {
        0 => "app".to_string(),
        1 => "data".to_string(),
        2 => "bootloader".to_string(),
        3 => "partition_table".to_string(),
        k => format!("{k:#04x}"),
    };
    let subtype = match (p.kind, p.subtype) {
        (0, 0x00) => "factory".to_string(),
        (0, s @ 0x10..=0x1F) => format!("ota_{}", s - 0x10),
        (0, 0x20) => "test".to_string(),
        (1, 0x00) => "ota".to_string(),
        (1, 0x01) => "phy".to_string(),
        (1, 0x02) => "nvs".to_string(),
        (1, 0x03) => "coredump".to_string(),
        (1, 0x04) => "nvs_keys".to_string(),
        (1, 0x05) => "efuse".to_string(),
        (1, 0x06) => "undefined".to_string(),
        (1, 0x80) => "esphttpd".to_string(),
        (1, 0x81) => "fat".to_string(),
        (1, 0x82) => "spiffs".to_string(),
        (1, 0x83) => "littlefs".to_string(),
        (2 | 3, 0x00) => "primary".to_string(),
        (2 | 3, 0x01) => "ota".to_string(),
        (_, s) => format!("{s:#04x}"),
    };
    format!("{kind}/{subtype}")
}

/// The files of a flash dump with a partition table: the bootloader, the
/// partition table, and the partitions in the table's order.
pub fn entries(data: &[u8]) -> Option<Vec<Entry>> {
    let (base, parts) = image::partition_table(data)?;
    let table = PARTITION_TABLE_OFFSET - base;
    let in_dump = |start: usize, size: usize| {
        (start < data.len()).then(|| start..start.saturating_add(size).min(data.len()))
    };

    // At 0x1000 on the ESP32 and ESP32-S2, at 0 on the others; it ends
    // where the partition table starts. Where no image is found there
    // (encrypted flash), the file starts at the start of the dump.
    let before_table = &data[..table.min(data.len())];
    let boot = [0x1000usize.checked_sub(base), (base == 0).then_some(0)]
        .into_iter()
        .flatten()
        .find(|&start| image::parse_image(before_table, start).is_some())
        .unwrap_or(0);
    let mut out = vec![
        Entry {
            name: BOOTLOADER.into(),
            flash_offset: (base + boot) as u32,
            size: (table - boot) as u32,
            range: Some(boot..table),
            partition: None,
        },
        Entry {
            name: PARTITION_TABLE.into(),
            flash_offset: PARTITION_TABLE_OFFSET as u32,
            size: TABLE_SECTOR as u32,
            range: in_dump(table, TABLE_SECTOR),
            partition: None,
        },
    ];
    // Labels are unique in tables gen_esp32part.py writes; a repeated label
    // (or one named like the bootloader or table) gets its offset added, and
    // then a number, until the name is unique.
    let mut names: HashSet<String> = out.iter().map(|e| e.name.clone()).collect();
    for p in parts {
        let mut name = p.label.clone();
        let mut n = 1;
        while names.contains(&name) {
            name = match n {
                1 => format!("{}@{:#x}", p.label, p.offset),
                n => format!("{}@{:#x}.{n}", p.label, p.offset),
            };
            n += 1;
        }
        names.insert(name.clone());
        out.push(Entry {
            name,
            flash_offset: p.offset,
            size: p.size,
            range: in_dump(p.offset as usize - base, p.size as usize),
            partition: Some(p),
        });
    }
    Some(out)
}

/// The file of a dump that is opened when no partition is chosen: the
/// factory app, else the first OTA app, with an image; else the bootloader.
pub fn default_entry<'a>(data: &[u8], entries: &'a [Entry]) -> Option<&'a Entry> {
    let parts: Vec<Partition> = entries.iter().filter_map(|e| e.partition.clone()).collect();
    let apps = image::app_partitions(&parts);
    apps.iter()
        .filter_map(|app| entries.iter().find(|e| e.partition.as_ref() == Some(app)))
        .chain(entries.iter().filter(|e| e.name == BOOTLOADER))
        .find(|e| e.has_image(data))
}

/// What the `files.espressif.partition` setting chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice<'a> {
    /// This file.
    One(&'a Entry),
    /// All files ([`ALL`]).
    All,
    /// Nothing that can be opened: the reason why.
    Invalid(&'static str),
}

/// The file of a dump that `choice`, the setting's value, chooses.
pub fn choose<'a>(data: &[u8], entries: &'a [Entry], choice: &str) -> Choice<'a> {
    let entry = match choice {
        ALL => return Choice::All,
        "" => match default_entry(data, entries) {
            Some(e) => e,
            None => return Choice::Invalid("the dump has no app or bootloader image"),
        },
        name => match entries.iter().find(|e| e.name == name) {
            Some(e) => e,
            None => return Choice::Invalid("the dump has no such partition"),
        },
    };
    if entry.range.is_none() {
        return Choice::Invalid("the partition is not in the dump");
    }
    Choice::One(entry)
}

pub struct EspFlashTransform;

impl EspFlashTransform {
    /// Lists the files of the dump, then, once asked, extracts the requested
    /// ones.
    fn split(context: &TransformContext) -> bool {
        let input = context.input();
        let data = input.read_vec(0, input.len() as usize);
        let Some(entries) = entries(&data) else {
            context.set_transform_result(TransformResult::TransformFailure);
            return false;
        };
        if context.available_files().is_empty() {
            let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            if !binaryninja::is_ui_enabled() {
                let choice = context.settings().get_string(PARTITION_SETTING);
                match choose(&data, &entries, &choice) {
                    Choice::One(e) => names = vec![e.name.as_str()],
                    Choice::All => {}
                    Choice::Invalid(why) => tracing::error!(
                        "{PARTITION_SETTING} '{choice}': {why}; opening the whole flash dump"
                    ),
                }
            }
            context.set_available_files(&names);
            return false;
        }

        let requested = context.requested_files();
        if requested.is_empty() {
            return false;
        }
        let mut complete = true;
        for name in requested.iter() {
            let name = name.to_string();
            let Some(entry) = entries.iter().find(|e| e.name == name) else {
                complete = false;
                continue;
            };
            let (bytes, result) = match entry.bytes(&data) {
                Some(b) => (b, TransformResult::TransformSuccess),
                None => {
                    complete = false;
                    (&[][..], TransformResult::TransformFailure)
                }
            };
            let child = context.create_child(
                &name,
                &DataBuffer::new(bytes),
                result,
                &entry.message(),
                false,
            );
            if result == TransformResult::TransformSuccess {
                record_extracted(context, entry, bytes);
            }
            let md = child.metadata();
            for (key, value) in entry.metadata() {
                let _ = md.insert(key, &value);
            }
        }
        complete
    }
}

impl CustomTransform for EspFlashTransform {
    const TYPE: TransformType = TransformType::DecodeTransform;
    const NAME: &'static str = "EspFlash";
    const LONG_NAME: &'static str = "Espressif Flash Dump";
    const GROUP: &'static str = "Container";

    // A container transform: its files are extracted in the context of a
    // session, not decoded from a buffer.
    fn decode(&self, _input: &[u8], _params: &TransformInputParameters) -> Option<DataBuffer> {
        None
    }

    fn encode(&self, _input: &[u8], _params: &TransformInputParameters) -> Option<DataBuffer> {
        None
    }
}

// The bindings call these from the core without catching panics, which
// would abort Binary Ninja; they are caught here.

impl DetectionTransform for EspFlashTransform {
    /// Is there a partition table at flash offset 0x8000, for a dump
    /// starting at flash offset 0 or 0x1000?
    fn can_decode(&self, reader: &mut BinaryReader) -> bool {
        catch_unwind(AssertUnwindSafe(|| {
            let Ok(len) = reader.seek(SeekFrom::End(0)) else {
                return false;
            };
            DUMP_BASES.iter().any(|&base| {
                let start = (PARTITION_TABLE_OFFSET - base) as u64;
                let mut table =
                    vec![0; (PARTITION_TABLE_SIZE as u64).min(len.saturating_sub(start)) as usize];
                !table.is_empty()
                    && reader.seek(SeekFrom::Start(start)).is_ok()
                    && reader.read_exact(&mut table).is_ok()
                    && image::parse_partition_table(&table).is_some()
            })
        }))
        .unwrap_or(false)
    }
}

impl ContextTransform for EspFlashTransform {
    fn decode_within_context(
        &self,
        context: &TransformContext,
        _params: &TransformInputParameters,
    ) -> bool {
        catch_unwind(AssertUnwindSafe(|| Self::split(context))).unwrap_or_else(|_| {
            tracing::error!("splitting the flash dump failed");
            false
        })
    }
}

/// A file this transform has extracted in this session.
struct Extracted {
    /// How its virtual path ends: Binary Ninja names a file of a container
    /// `<transform>(<container's name>)::<file's name>`, after the path of
    /// the container's own container, if any (`Zip(a.zip)::EspFlash(d.bin)::nvs`).
    path_suffix: String,
    /// The SHA-256 of its contents.
    digest: [u8; 32],
    entry: Entry,
}

/// The files extracted in this session, so that the view of one can tell
/// where in flash it came from.
static EXTRACTED: Mutex<Vec<Extracted>> = Mutex::new(Vec::new());

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn record_extracted(context: &TransformContext, entry: &Entry, bytes: &[u8]) {
    let Ok(mut extracted) = EXTRACTED.lock() else {
        return;
    };
    let path_suffix = format!(
        "{}({})::{}",
        EspFlashTransform::NAME,
        context.name(),
        entry.name
    );
    let digest = digest(bytes);
    if !extracted
        .iter()
        .any(|e| e.path_suffix == path_suffix && e.digest == digest && e.entry == *entry)
    {
        extracted.push(Extracted {
            path_suffix,
            digest,
            entry: entry.clone(),
        });
    }
}

/// Where the file of `view` (a raw view) came from, if this transform
/// extracted it from a flash dump: its virtual path and name, its flash
/// offset and size, and its partition's label, type and subtype. (If files
/// with the same path and contents were extracted from differing dumps in
/// this session, only the path and name.)
///
/// The first time, it is worked out from the files extracted in this
/// session and stored in the view's metadata, since a database keeps
/// neither the virtual path nor the session; a database's views are created
/// before their metadata is restored, except for the raw view's.
pub fn entry_metadata(view: &BinaryView) -> Option<Ref<Metadata>> {
    if let Some(md) = view.query_metadata(ENTRY_METADATA) {
        return Some(md);
    }
    let path = view.file().virtual_path()?;
    let mut md: HashMap<&str, Ref<Metadata>> = HashMap::new();
    {
        let extracted = EXTRACTED.lock().ok()?;
        let mut digest_of_view = None;
        let mut matches = extracted.iter().filter(|e| {
            path.ends_with(&e.path_suffix)
                && e.digest
                    == *digest_of_view
                        .get_or_insert_with(|| digest(&view.read_vec(0, view.len() as usize)))
        });
        let first = matches.next()?;
        md.insert("virtualPath", path.as_str().into());
        md.insert("name", first.entry.name.as_str().into());
        if matches.all(|e| e.entry == first.entry) {
            md.extend(first.entry.metadata());
        }
    }
    let md: Ref<Metadata> = md.into();
    view.store_metadata(ENTRY_METADATA, md.clone(), MetadataStoreFlags::PERSISTENT);
    Some(md)
}

/// Registers the transform and the setting that chooses its file headless.
pub fn register() {
    let schema = serde_json::json!({
        "title": "Espressif Flash Dump Partition",
        "type": "string",
        "default": "",
        "description": "The file of an Espressif flash dump to open when Binary Ninja runs \
            headless (the UI offers them all): a partition's label, 'bootloader' or \
            'partition-table'; empty for the factory app (or else the first OTA app); \
            or '*' to open the whole dump. Where the whole dump is opened (with '*', or \
            without the EspFlash container transform), the image of the app partition or \
            bootloader it names is loaded.",
        "ignore": ["SettingsProjectScope", "SettingsResourceScope"],
    });
    Settings::global().register_setting_json(PARTITION_SETTING, &schema.to_string());
    register_transform_with_detection_and_context(EspFlashTransform);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-segment ESP32-S3 image.
    fn app(entry: u32) -> Vec<u8> {
        let mut v = vec![0xE9, 1, 2, 0x20];
        v.extend(entry.to_le_bytes());
        v.extend([0xEE, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        v.extend(entry.to_le_bytes());
        v.extend(16u32.to_le_bytes());
        v.extend([0; 16]);
        v
    }

    /// A dump starting at flash offset `base`, of `len` bytes of flash,
    /// with these `(type, subtype, offset, size, label)` partitions and
    /// these images at these flash offsets.
    fn dump(
        base: usize,
        len: usize,
        parts: &[(u8, u8, u32, u32, &str)],
        images: &[(usize, Vec<u8>)],
    ) -> Vec<u8> {
        let mut d = vec![0xFF; len - base];
        for (i, &(kind, subtype, offset, size, label)) in parts.iter().enumerate() {
            let mut entry = vec![0xAA, 0x50, kind, subtype];
            entry.extend(offset.to_le_bytes());
            entry.extend(size.to_le_bytes());
            let mut name = [0u8; 16];
            name[..label.len()].copy_from_slice(label.as_bytes());
            entry.extend(name);
            entry.extend([0; 4]);
            let at = PARTITION_TABLE_OFFSET - base + 32 * i;
            d[at..at + 32].copy_from_slice(&entry);
        }
        for (offset, image) in images {
            d[offset - base..offset - base + image.len()].copy_from_slice(image);
        }
        d
    }

    const LAYOUT: &[(u8, u8, u32, u32, &str)] = &[
        (1, 2, 0x9000, 0x6000, "nvs"),
        (1, 1, 0xF000, 0x1000, "phy_init"),
        (0, 0, 0x10000, 0x10000, "factory"),
        (0, 0x10, 0x20000, 0x10000, "ota_0"),
    ];

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn files_of_a_dump() {
        // Like `esptool merge_bin` output for the ESP32: from flash 0x1000.
        let d = dump(0x1000, 0x30000, LAYOUT, &[(0x1000, app(0x4037_5000))]);
        let entries = entries(&d).unwrap();
        assert_eq!(
            names(&entries),
            [
                "bootloader",
                "partition-table",
                "nvs",
                "phy_init",
                "factory",
                "ota_0"
            ]
        );
        let boot = &entries[0];
        assert_eq!(
            (boot.flash_offset, boot.size, boot.range.clone()),
            (0x1000, 0x7000, Some(0..0x7000))
        );
        assert_eq!(entries[1].range, Some(0x7000..0x8000));
        let factory = &entries[4];
        assert_eq!(factory.range, Some(0xF000..0x1F000));
        assert_eq!(factory.partition.as_ref().unwrap().subtype, 0);
        assert_eq!(
            factory.message(),
            "app/factory partition at 0x10000, 0x10000 bytes"
        );
        assert_eq!(
            entries[2].message(),
            "data/nvs partition at 0x9000, 0x6000 bytes"
        );
        assert_eq!(
            boot.message(),
            "second stage bootloader at 0x1000, 0x7000 bytes"
        );
        assert_eq!(
            entries[1].message(),
            "partition table at 0x8000, 0x1000 bytes"
        );
    }

    #[test]
    fn bootloader_at_0_or_0x1000() {
        // From flash 0: the ESP32-S3 and later have their bootloader at 0,
        // the ESP32 and ESP32-S2 at 0x1000.
        for at in [0, 0x1000] {
            let d = dump(0, 0x30000, LAYOUT, &[(at, app(0x4037_5000))]);
            let boot = &entries(&d).unwrap()[0];
            assert_eq!(
                (boot.flash_offset as usize, boot.range.clone()),
                (at, Some(at..0x8000))
            );
        }
        // No image: all that comes before the table.
        let d = dump(0, 0x30000, LAYOUT, &[]);
        assert_eq!(entries(&d).unwrap()[0].range, Some(0..0x8000));
    }

    #[test]
    fn partitions_past_the_end_of_the_dump() {
        // The dump ends inside ota_0.
        let d = dump(0, 0x28000, LAYOUT, &[]);
        let files = entries(&d).unwrap();
        let ota = files.iter().find(|e| e.name == "ota_0").unwrap();
        assert_eq!(ota.range, Some(0x20000..0x28000));
        assert!(ota.message().ends_with("the dump ends after 0x8000 bytes"));
        // ... and before it.
        let d = dump(0, 0x20000, LAYOUT, &[]);
        let files = entries(&d).unwrap();
        let ota = files.iter().find(|e| e.name == "ota_0").unwrap();
        assert_eq!(ota.range, None);
        assert!(ota.message().ends_with("not in the dump"));
        assert_eq!(
            choose(&d, &files, "ota_0"),
            Choice::Invalid("the partition is not in the dump")
        );
    }

    #[test]
    fn repeated_names() {
        let parts = [
            (0, 0, 0x10000, 0x1000, "app"),
            (0, 0x10, 0x11000, 0x1000, "app"),
            (2, 0, 0x12000, 0x1000, "bootloader"),
        ];
        let d = dump(0, 0x20000, &parts, &[]);
        assert_eq!(
            names(&entries(&d).unwrap()),
            [
                "bootloader",
                "partition-table",
                "app",
                "app@0x11000",
                "bootloader@0x12000"
            ]
        );
        // A label like a name made for another, and a label repeated at the
        // same offset: every name is unique.
        let parts = [
            (0, 0, 0x10000, 0x1000, "app"),
            (0, 0x10, 0x11000, 0x1000, "app"),
            (0, 0x11, 0x12000, 0x1000, "app@0x11000"),
            (0, 0x12, 0x11000, 0x1000, "app"),
        ];
        let d = dump(0, 0x20000, &parts, &[]);
        let files = entries(&d).unwrap();
        assert_eq!(
            names(&files)[2..],
            ["app", "app@0x11000", "app@0x11000@0x12000", "app@0x11000.2"]
        );
        let subtypes: Vec<u8> = files[2..]
            .iter()
            .map(|e| e.partition.as_ref().unwrap().subtype)
            .collect();
        assert_eq!(subtypes, [0, 0x10, 0x11, 0x12]);
    }

    #[test]
    fn choices() {
        let chosen = |d: &[u8], choice: &str| {
            let entries = entries(d).unwrap();
            match choose(d, &entries, choice) {
                Choice::One(e) => Ok(e.name.clone()),
                Choice::All => Ok(ALL.into()),
                Choice::Invalid(why) => Err(why),
            }
        };
        let boot = (0, app(0x4037_5008));
        let factory = (0x10000, app(0x4037_5000));
        let ota = (0x20000, app(0x4037_5004));
        let d = dump(0, 0x30000, LAYOUT, &[boot.clone(), factory, ota.clone()]);
        assert_eq!(chosen(&d, ""), Ok("factory".into()));
        assert_eq!(chosen(&d, "ota_0"), Ok("ota_0".into()));
        assert_eq!(chosen(&d, "nvs"), Ok("nvs".into()));
        assert_eq!(chosen(&d, BOOTLOADER), Ok(BOOTLOADER.into()));
        assert_eq!(chosen(&d, PARTITION_TABLE), Ok(PARTITION_TABLE.into()));
        assert_eq!(chosen(&d, ALL), Ok(ALL.into()));
        assert_eq!(chosen(&d, "nope"), Err("the dump has no such partition"));
        // An empty factory partition: the first OTA app.
        let d = dump(0, 0x30000, LAYOUT, &[boot.clone(), ota]);
        assert_eq!(chosen(&d, ""), Ok("ota_0".into()));
        // No app image: the bootloader; nor that: nothing.
        let d = dump(0, 0x30000, LAYOUT, &[boot]);
        assert_eq!(chosen(&d, ""), Ok(BOOTLOADER.into()));
        let d = dump(0, 0x30000, LAYOUT, &[]);
        assert_eq!(
            chosen(&d, ""),
            Err("the dump has no app or bootloader image")
        );
    }

    #[test]
    fn type_names() {
        let name = |kind, subtype| {
            partition_type_name(&Partition {
                kind,
                subtype,
                offset: 0,
                size: 0,
                label: String::new(),
            })
        };
        assert_eq!(name(0, 0x00), "app/factory");
        assert_eq!(name(0, 0x13), "app/ota_3");
        assert_eq!(name(1, 0x82), "data/spiffs");
        assert_eq!(name(0x40, 0x07), "0x40/0x07");
    }
}
