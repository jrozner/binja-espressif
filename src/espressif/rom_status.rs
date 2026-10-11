//! What a view's ROM code region holds, as view metadata a script can read
//! in one call: `bv.query_metadata("espressif.rom")`.
//!
//! ```text
//! {
//!   "status":   "mapped" | "zeros" | "downloading" | "failed",
//!   "chip":     "esp32c3",                 // the platform name
//!   // only with "mapped":
//!   "image":    "esp32c3_rev3_rom.elf",    // the ROM ELF file's name
//!   "revision": 3,                         // its revision, major * 100 + minor
//!   "source":   "elfDirectory" | "esp-idf" | "cache",
//!   "release":  "20260528",                // the esp-rom-elfs version, except from elfDirectory
//!   "sha256":   "…",                       // the ROM ELF file's SHA-256
//!   "dataAlias": true                      // the ROM's data alias is mapped (espressif.rom.data)
//! }
//! ```
//!
//! * `mapped`: the region holds the ROM image `image`.
//! * `zeros`: it holds zeros: no image was found, or `loadImages` is off.
//!   An image downloaded or configured later is mapped when the view is
//!   next analyzed (or, for a database, by the download command).
//! * `downloading`: zeros, while a download of the ROM images runs.
//! * `failed`: zeros, and this session's last download failed or timed
//!   out (the log says why).
//!
//! `dataAlias` says whether the image's read-only data is mapped where the
//! data bus sees it, as the `espressif.rom.data` region (see
//! [`rom_types::add_rom_data_region`]): not where the file maps that range
//! itself.
//!
//! A view without the key has no ROM code region: its chip has no ROM
//! types (the ESP8266), or the file maps the ROM itself. The older
//! `espressif.rom.image` key (the image's name, absent with zeros) is kept.
//!
//! The metadata is saved in databases, so it holds no absolute path, which
//! would carry the user's name and directory layout along with a shared
//! database: the file is `image` in the `espressif.rom.elfDirectory`
//! directory, in `<user dir>/espressif/rom-elfs/<release>/` (`cache`), or
//! in `<IDF_TOOLS_PATH or ~/.espressif>/tools/esp-rom-elfs/<release>/`
//! (`esp-idf`), and the log names it. `downloading` and `failed` describe
//! the session: a database saved with them says `zeros` once it is
//! analyzed again in a session without a download.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use binaryninja::binary_view::{BinaryView, MetadataStoreFlags};
use binaryninja::metadata::Metadata;
use binaryninja::rc::Ref;

use crate::espressif::rom_images::{Found, parse_member_name};
use crate::espressif::{Chip, rom_download, rom_types};

/// The view metadata key.
pub const METADATA: &str = "espressif.rom";

/// What the ROM code region holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Mapped,
    Zeros,
    Downloading,
    Failed,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Mapped => "mapped",
            Status::Zeros => "zeros",
            Status::Downloading => "downloading",
            Status::Failed => "failed",
        }
    }

    /// The status of a region of zeros, given whether a download runs and
    /// whether this session's last one failed.
    pub fn unmapped(downloading: bool, failed: bool) -> Status {
        match () {
            _ if downloading => Status::Downloading,
            _ if failed => Status::Failed,
            _ => Status::Zeros,
        }
    }
}

/// Which of the searched directories a ROM image came from (see
/// [`rom_images::search_dirs`](super::rom_images::search_dirs)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    ElfDirectory,
    EspIdf,
    Cache,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::ElfDirectory => "elfDirectory",
            Source::EspIdf => "esp-idf",
            Source::Cache => "cache",
        }
    }

    /// The source of the image at `path`, found with `user` as the values
    /// of the `elfDirectory` setting and `cache` as the download cache, and
    /// the esp-rom-elfs release its directory is named for (none for the
    /// user's directories). The user's directories come first, as in the
    /// search.
    pub fn of(path: &Path, user: &[&str], cache: &Path) -> (Source, Option<String>) {
        let dir = path.parent().unwrap_or(Path::new(""));
        let release = || dir.file_name().map(|n| n.to_string_lossy().into_owned());
        let user_dir = |u: &&str| !u.trim().is_empty() && dir == Path::new(u.trim());
        if user.iter().any(user_dir) {
            (Source::ElfDirectory, None)
        } else if dir == cache {
            (Source::Cache, release())
        } else {
            (Source::EspIdf, release())
        }
    }
}

/// A metadata value: a string or an unsigned integer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Num(u64),
    Bool(bool),
}

/// The metadata of a view's ROM code region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomStatus {
    pub status: Status,
    pub chip: Chip,
    pub image: Option<Image>,
}

/// The image a `mapped` region holds. What an image mapped by an older
/// version of the plugin (which recorded only its name) does not say is
/// absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub name: String,
    pub revision: Option<u16>,
    pub source: Option<Source>,
    pub release: Option<String>,
    pub sha256: Option<String>,
    pub data_alias: Option<bool>,
}

impl RomStatus {
    /// The metadata's fields.
    pub fn fields(&self) -> Vec<(&'static str, Value)> {
        let mut out = vec![
            ("status", Value::Str(self.status.name().into())),
            ("chip", Value::Str(self.chip.platform_name().into())),
        ];
        if let Some(image) = &self.image {
            out.push(("image", Value::Str(image.name.clone())));
            let optional = [
                ("revision", image.revision.map(|r| Value::Num(r as u64))),
                ("source", image.source.map(|s| Value::Str(s.name().into()))),
                ("release", image.release.clone().map(Value::Str)),
                ("sha256", image.sha256.clone().map(Value::Str)),
                ("dataAlias", image.data_alias.map(Value::Bool)),
            ];
            out.extend(optional.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))));
        }
        out
    }
}

/// Does the stored metadata have exactly these fields?
fn same(stored: &HashMap<String, Ref<Metadata>>, fields: &[(&'static str, Value)]) -> bool {
    stored.len() == fields.len()
        && fields.iter().all(|(k, v)| {
            stored.get(*k).is_some_and(|m| match v {
                Value::Str(s) => String::try_from(m.as_ref()).is_ok_and(|m| m == *s),
                Value::Num(n) => u64::try_from(m.as_ref()) == Ok(*n),
                Value::Bool(b) => bool::try_from(m.as_ref()) == Ok(*b),
            })
        })
}

/// Held while the metadata is written, so that a region of zeros is not
/// recorded over an image mapped meanwhile (the download runs on a thread
/// of its own, and analysis may map an image it has unpacked).
static WRITING: Mutex<()> = Mutex::new(());

/// Stores the view's ROM metadata, saved with its database, unless it is
/// stored already (this runs on every analysis update). A status other
/// than `mapped` is recorded only while the region holds zeros: the image
/// is recorded (`espressif.rom.image`) before its status, so a status
/// written after that sees it, and one written before is overwritten.
pub fn record(view: &BinaryView, status: &RomStatus) {
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    if status.status != Status::Mapped && !rom_types::has_zero_filled_region(view, status.chip) {
        return;
    }
    let fields = status.fields();
    let stored = view
        .query_metadata(METADATA)
        .and_then(|m| HashMap::try_from(m.as_ref()).ok());
    if stored.is_some_and(|s| same(&s, &fields)) {
        return;
    }
    let value: HashMap<&str, Ref<Metadata>> = fields
        .into_iter()
        .map(|(k, v)| {
            let v: Ref<Metadata> = match v {
                Value::Str(s) => s.into(),
                Value::Num(n) => n.into(),
                Value::Bool(b) => b.into(),
            };
            (k, v)
        })
        .collect();
    view.store_metadata(METADATA, value, MetadataStoreFlags::PERSISTENT);
}

/// Is the ROM's data alias mapped?
fn data_alias(view: &BinaryView) -> bool {
    view.memory_map()
        .get_region(rom_types::DATA_REGION)
        .is_some()
}

/// Records that the view's ROM code region holds the image `found`.
pub fn record_mapped(view: &BinaryView, chip: Chip, found: &Found) {
    let user = rom_download::elf_directories(view);
    let user = user.each_ref().map(String::as_str);
    let (source, release) = Source::of(&found.path, &user, &rom_download::cache_dir());
    let name = found
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let image = Image {
        name,
        revision: Some(found.revision),
        source: Some(source),
        release,
        sha256: Some(found.sha256.clone()),
        data_alias: Some(data_alias(view)),
    };
    record(
        view,
        &RomStatus {
            status: Status::Mapped,
            chip,
            image: Some(image),
        },
    );
}

/// Records that the view's ROM code region holds zeros: `zeros`,
/// `downloading` or `failed`, by the state of this session's download.
pub fn record_unmapped(view: &BinaryView, chip: Chip) {
    let (downloading, failed) = rom_download::download_state();
    record_status(view, chip, Status::unmapped(downloading, failed));
}

/// Records a region of zeros with status `status`.
pub fn record_status(view: &BinaryView, chip: Chip, status: Status) {
    record(
        view,
        &RomStatus {
            status,
            chip,
            image: None,
        },
    );
}

/// Brings a mapped image's `dataAlias` up to date: the command maps the
/// alias into a database saved without it, and a script may remove it.
/// Runs on every analysis update; writes only when it changed.
pub fn sync_data_alias(view: &BinaryView) {
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut stored) = view
        .query_metadata(METADATA)
        .and_then(|m| HashMap::<String, Ref<Metadata>>::try_from(m.as_ref()).ok())
    else {
        return;
    };
    let mapped = stored
        .get("status")
        .is_some_and(|s| String::try_from(s.as_ref()).is_ok_and(|s| s == Status::Mapped.name()));
    let actual = data_alias(view);
    let recorded = stored
        .get("dataAlias")
        .and_then(|d| bool::try_from(d.as_ref()).ok());
    if !mapped || recorded == Some(actual) {
        return;
    }
    stored.insert("dataAlias".into(), actual.into());
    view.store_metadata(METADATA, stored, MetadataStoreFlags::PERSISTENT);
}

/// For a database saved by a version of the plugin that recorded only the
/// name of the image its region holds: records what the name says.
pub fn backfill(view: &BinaryView, chip: Chip) {
    if view.query_metadata(METADATA).is_some() {
        return;
    }
    let Some(name) = rom_types::held_image(view) else {
        return;
    };
    let revision = parse_member_name(&name).map(|(_, rev)| rev);
    let image = Image {
        name,
        revision,
        source: None,
        release: None,
        sha256: None,
        data_alias: Some(data_alias(view)),
    };
    record(
        view,
        &RomStatus {
            status: Status::Mapped,
            chip,
            image: Some(image),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn unmapped_status() {
        assert_eq!(Status::unmapped(false, false), Status::Zeros);
        assert_eq!(Status::unmapped(true, false), Status::Downloading);
        assert_eq!(Status::unmapped(true, true), Status::Downloading);
        assert_eq!(Status::unmapped(false, true), Status::Failed);
    }

    #[test]
    fn sources() {
        let cache = PathBuf::from("/bn/espressif/rom-elfs/20260528");
        let of = |p: &str, user: &str| Source::of(Path::new(p), &[user, "/global"], &cache);
        assert_eq!(
            of("/my/roms/esp32_rev0_rom.elf", " /my/roms/ "),
            (Source::ElfDirectory, None)
        );
        assert_eq!(
            of("/global/esp32_rev0_rom.elf", ""),
            (Source::ElfDirectory, None)
        );
        assert_eq!(
            of("/bn/espressif/rom-elfs/20260528/esp32_rev0_rom.elf", ""),
            (Source::Cache, Some("20260528".into()))
        );
        assert_eq!(
            of(
                "/home/me/.espressif/tools/esp-rom-elfs/20240305/esp32_rev0_rom.elf",
                "/my/roms"
            ),
            (Source::EspIdf, Some("20240305".into()))
        );
        // The user's directory set to the cache: the user's, as searched.
        assert_eq!(
            of(
                "/bn/espressif/rom-elfs/20260528/esp32_rev0_rom.elf",
                "/bn/espressif/rom-elfs/20260528"
            ),
            (Source::ElfDirectory, None)
        );
    }

    #[test]
    fn fields() {
        let zeros = RomStatus {
            status: Status::Failed,
            chip: Chip::Esp32C3,
            image: None,
        };
        assert_eq!(
            zeros.fields(),
            [
                ("status", Value::Str("failed".into())),
                ("chip", Value::Str("esp32c3".into())),
            ]
        );
        let mapped = RomStatus {
            status: Status::Mapped,
            chip: Chip::Esp32,
            image: Some(Image {
                name: "esp32_rev300_rom.elf".into(),
                revision: Some(300),
                source: Some(Source::EspIdf),
                release: Some("20240305".into()),
                sha256: Some("ab".into()),
                data_alias: Some(true),
            }),
        };
        let keys: Vec<&str> = mapped.fields().iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            [
                "status",
                "chip",
                "image",
                "revision",
                "source",
                "release",
                "sha256",
                "dataAlias"
            ]
        );
        assert!(mapped.fields().contains(&("revision", Value::Num(300))));
        assert!(
            mapped
                .fields()
                .contains(&("source", Value::Str("esp-idf".into())))
        );
        // An image an older version recorded: only what its name says.
        let old = RomStatus {
            image: Some(Image {
                name: "esp32_rev0_rom.elf".into(),
                revision: Some(0),
                source: None,
                release: None,
                sha256: None,
                data_alias: None,
            }),
            ..mapped
        };
        assert_eq!(old.fields().len(), 4);
    }
}
