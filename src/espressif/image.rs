//! Espressif firmware image formats and memory maps.
//!
//! Supported layouts:
//!
//! * **ESP32 / ESP32-S2 / ESP32-S3 images** (all ESP-IDF versions, apps and
//!   second stage bootloaders): an 8-byte header with magic `0xE9`, a 16-byte
//!   extended header whose `chip_id` selects the chip (`0` for the ESP32,
//!   which is also what images written before the field existed contain),
//!   then `count` segments of `(load address, length, data)`. esptool pads
//!   flash-mapped segments with segments whose load address is 0.
//! * **ESP8266 v1 images** (NONOS SDK `eagle.flash.bin`, RTOS SDK): the same
//!   without the extended header. The flash-mapped code (`irom0`) is usually
//!   a separate file flashed at 0x10000/0x40000 and mapped at
//!   `0x40200000 + flash offset`.
//! * **ESP8266 v2 images** (magic `0xEA`, NONOS OTA `user1.bin`, Arduino):
//!   the `irom0` segment comes first, followed by a v1 image of the RAM
//!   segments. The `irom0` data is mapped at `0x40200000 + flash offset`.
//! * **Flash dumps**: only one image of the partition table at 0x8000 is
//!   loaded: the factory (or first OTA) application, or the app partition or
//!   bootloader selected by the view's `loader.esp.partition` load setting. ESP8266
//!   dumps without a partition table (NONOS SDK) are additionally mapped as
//!   a whole at the flash cache window `0x40200000`, where `irom0` lives.

use crate::espressif::Chip;
use crate::xtensa;

pub const IMAGE_MAGIC: u8 = 0xE9;
pub const IMAGE_V2_MAGIC: u8 = 0xEA;
pub const APP_DESC_MAGIC: u32 = 0xABCD_5432;
/// `sizeof(esp_app_desc_t)`.
pub const APP_DESC_SIZE: usize = 256;
pub const ESP8266_FLASH_MAP: u32 = 0x4020_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Rom,
    Iram,
    Dram,
    /// Flash mapped, executable.
    Irom,
    /// Flash mapped, read-only data.
    Drom,
    RtcCode,
    RtcData,
    ExtRam,
    Peripheral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub name: &'static str,
    pub start: u32,
    pub end: u32,
    pub kind: RegionKind,
}

impl Region {
    pub fn contains(&self, addr: u32) -> bool {
        self.start <= addr && addr < self.end
    }

    pub fn is_code(&self) -> bool {
        matches!(
            self.kind,
            RegionKind::Iram | RegionKind::Irom | RegionKind::RtcCode | RegionKind::Rom
        )
    }
}

const fn r(name: &'static str, start: u32, end: u32, kind: RegionKind) -> Region {
    Region {
        name,
        start,
        end,
        kind,
    }
}

use RegionKind as K;

// From ESP-IDF components/soc/<chip>/include/soc/soc.h and the ESP8266 RTOS
// SDK's ld scripts.
static ESP8266_REGIONS: &[Region] = &[
    r("dport", 0x3FF0_0000, 0x3FF0_1000, K::Peripheral),
    r("dram", 0x3FFE_8000, 0x4000_0000, K::Dram),
    r("rom", 0x4000_0000, 0x4001_0000, K::Rom),
    r("iram", 0x4010_0000, 0x4011_0000, K::Iram),
    r("irom", 0x4020_0000, 0x4040_0000, K::Irom),
    r("peripherals", 0x6000_0000, 0x6000_1000, K::Peripheral),
    r("rtc_data", 0x6000_1000, 0x6000_2000, K::RtcData),
];

static ESP32_REGIONS: &[Region] = &[
    r("drom", 0x3F40_0000, 0x3F80_0000, K::Drom),
    r("extram", 0x3F80_0000, 0x3FC0_0000, K::ExtRam),
    r("peripherals", 0x3FF0_0000, 0x3FF8_0000, K::Peripheral),
    r("rtc_dram", 0x3FF8_0000, 0x3FF8_2000, K::RtcData),
    r("dram", 0x3FFA_E000, 0x4000_0000, K::Dram),
    r("rom", 0x4000_0000, 0x4007_0000, K::Rom),
    r("iram", 0x4007_0000, 0x400C_0000, K::Iram),
    r("rtc_iram", 0x400C_0000, 0x400C_2000, K::RtcCode),
    r("irom", 0x400C_2000, 0x4040_0000, K::Irom),
    r("rtc_slow", 0x5000_0000, 0x5000_2000, K::RtcData),
];

static ESP32S2_REGIONS: &[Region] = &[
    r("drom", 0x3F00_0000, 0x3F40_0000, K::Drom),
    r("peripherals", 0x3F40_0000, 0x3F50_0000, K::Peripheral),
    r("extram", 0x3F50_0000, 0x3FF8_0000, K::ExtRam),
    r("rtc_dram", 0x3FF9_E000, 0x3FFA_0000, K::RtcData),
    r("dram", 0x3FFB_0000, 0x4000_0000, K::Dram),
    r("rom", 0x4000_0000, 0x4002_0000, K::Rom),
    r("iram", 0x4002_0000, 0x4007_0000, K::Iram),
    r("rtc_iram", 0x4007_0000, 0x4007_2000, K::RtcCode),
    r("irom", 0x4008_0000, 0x4080_0000, K::Irom),
    r("rtc_slow", 0x5000_0000, 0x5000_2000, K::RtcData),
];

static ESP32S3_REGIONS: &[Region] = &[
    r("drom", 0x3C00_0000, 0x3E00_0000, K::Drom),
    r("dram", 0x3FC8_0000, 0x3FD0_0000, K::Dram),
    r("rom", 0x4000_0000, 0x4006_0000, K::Rom),
    r("iram", 0x4037_0000, 0x403E_0000, K::Iram),
    r("irom", 0x4200_0000, 0x4400_0000, K::Irom),
    r("rtc_slow", 0x5000_0000, 0x5000_2000, K::RtcData),
    r("peripherals", 0x6000_0000, 0x600D_1000, K::Peripheral),
    r("rtc_fast", 0x600F_E000, 0x6010_0000, K::RtcCode),
];

pub fn regions(chip: Chip) -> &'static [Region] {
    match chip {
        Chip::Esp8266 => ESP8266_REGIONS,
        Chip::Esp32 => ESP32_REGIONS,
        Chip::Esp32S2 => ESP32S2_REGIONS,
        Chip::Esp32S3 => ESP32S3_REGIONS,
    }
}

pub fn region_of(chip: Chip, addr: u32) -> Option<&'static Region> {
    regions(chip).iter().find(|r| r.contains(addr))
}

/// The chip of an ESP-IDF image `chip_id`. Other ids belong to the RISC-V
/// chips (ESP32-C3, ...).
pub fn chip_from_id(id: u16) -> Option<Chip> {
    match id {
        0 => Some(Chip::Esp32),
        2 => Some(Chip::Esp32S2),
        9 => Some(Chip::Esp32S3),
        _ => None,
    }
}

/// Guesses the chip from the load addresses of a program, for formats that
/// do not say (ELF files, and images with a `chip_id` of 0, which is the
/// ESP32 or an ESP8266 RTOS SDK v3 image). `windowed_entry` says, if known,
/// whether the entry point starts with `entry`: ESP32-family code uses the
/// windowed ABI, whose functions start with `entry`, while the ESP8266 uses
/// `call0`.
pub fn guess_chip(addrs: &[u32], windowed_entry: Option<bool>) -> Chip {
    let any_in = |ranges: &[(u32, u32)]| {
        addrs
            .iter()
            .any(|&a| ranges.iter().any(|&(lo, hi)| lo <= a && a < hi))
    };
    // Regions that only exist on one chip.
    if any_in(&[
        (0x4200_0000, 0x4400_0000),
        (0x3C00_0000, 0x3E00_0000),
        (0x4037_0000, 0x403E_0000),
        (0x3FC8_0000, 0x3FD0_0000),
    ]) {
        return Chip::Esp32S3;
    }
    if any_in(&[(0x3F00_0000, 0x3F40_0000), (0x4002_0000, 0x4007_0000)]) {
        return Chip::Esp32S2;
    }
    if any_in(&[
        (0x3F40_0000, 0x3F80_0000),
        (0x3FF8_0000, 0x3FF8_2000),
        (0x400C_0000, 0x400C_2000),
    ]) {
        return Chip::Esp32;
    }
    // Without a windowed prologue at the entry point, the program is call0
    // code: ESP8266 code, unless it is loaded where the ESP8266 has no memory
    // (an ESP32 call0 startup stub, for instance).
    let fits_esp8266 = addrs.iter().all(|&a| region_of(Chip::Esp8266, a).is_some());
    match windowed_entry {
        Some(true) => Chip::Esp32,
        Some(false) if fits_esp8266 => Chip::Esp8266,
        Some(false) => Chip::Esp32,
        None if any_in(&[
            (0x4010_0000, 0x4011_0000),
            (0x3FFE_8000, 0x4000_0000),
            (0x4020_0000, 0x4030_0000),
        ]) =>
        {
            Chip::Esp8266
        }
        None => Chip::Esp32,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// ESP8266 image without extended header.
    Esp8266V1,
    /// ESP8266 image with the flash segment first (magic 0xEA).
    Esp8266V2,
    /// ESP32-family image (8-byte header + 16-byte extended header).
    Esp32,
}

/// A segment of an image: `size` bytes at file offset `offset` loaded at
/// `addr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub addr: u32,
    pub offset: usize,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub chip: Chip,
    pub format: Format,
    /// File offset of the image header.
    pub start: usize,
    pub entry: u32,
    pub segments: Vec<Segment>,
    /// The `chip_id` of the extended header.
    pub chip_id: Option<u16>,
    /// The minimum chip revision the image requires, as major * 100 + minor
    /// (0 when the extended header does not say).
    pub min_chip_rev: u16,
    pub hash_appended: bool,
}

impl Image {
    /// The address and bytes of the `esp_app_desc_t` of an ESP-IDF
    /// application, which is at the start of its first DROM segment.
    pub fn app_desc<'a>(&self, data: &'a [u8]) -> Option<(u32, &'a [u8])> {
        self.segments
            .iter()
            .filter(|s| region_of(self.chip, s.addr).is_some_and(|r| r.kind == K::Drom))
            .find(|s| read_u32(data, s.offset) == Some(APP_DESC_MAGIC))
            .filter(|s| s.size as usize >= APP_DESC_SIZE)
            .and_then(|s| Some((s.addr, data.get(s.offset..s.offset + APP_DESC_SIZE)?)))
    }
}

fn read_u32(data: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(off..off + 4)?.try_into().ok()?))
}

fn read_u16(data: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(off..off + 2)?.try_into().ok()?))
}

/// Reads `count` segment headers starting at `off`. Fails if any segment
/// extends past the end of the file or is implausibly large.
fn read_segments(data: &[u8], mut off: usize, count: usize) -> Option<(Vec<Segment>, usize)> {
    let mut segments = Vec::with_capacity(count);
    for _ in 0..count {
        let addr = read_u32(data, off)?;
        let size = read_u32(data, off + 4)?;
        if size > 0x0100_0000 || off + 8 + size as usize > data.len() {
            return None;
        }
        segments.push(Segment {
            addr,
            offset: off + 8,
            size,
        });
        off += 8 + size as usize;
    }
    Some((segments, off))
}

/// Do all segments (other than padding) and the entry point fall in the
/// chip's memory map, with the entry point in code?
fn plausible(chip: Chip, entry: u32, segments: &[Segment]) -> bool {
    let entry_ok = region_of(chip, entry).is_some_and(|r| r.is_code() && r.kind != K::Rom);
    let mut loaded = segments
        .iter()
        .filter(|s| s.addr != 0 && s.size != 0)
        .peekable();
    entry_ok
        && loaded.peek().is_some()
        && loaded.all(|s| {
            region_of(chip, s.addr).is_some_and(|r| s.addr as u64 + s.size as u64 <= r.end as u64)
        })
}

fn code_at<'a>(data: &'a [u8], segments: &[Segment], addr: u32) -> Option<&'a [u8]> {
    let s = segments
        .iter()
        .find(|s| s.addr <= addr && (addr as u64) < s.addr as u64 + s.size as u64)?;
    let off = s.offset + (addr - s.addr) as usize;
    data.get(off..(off + 3).min(s.offset + s.size as usize))
}

/// Parses the image whose header is at `start`.
pub fn parse_image(data: &[u8], start: usize) -> Option<Image> {
    let header = data.get(start..start + 8)?;
    let count = header[1] as usize;
    let entry = read_u32(header, 4)?;
    if count == 0 || count > 16 {
        return None;
    }
    match header[0] {
        IMAGE_MAGIC => parse_esp32(data, start, count, entry)
            .or_else(|| parse_esp8266_v1(data, start, count, entry)),
        IMAGE_V2_MAGIC => parse_esp8266_v2(data, start, entry),
        _ => None,
    }
}

fn parse_esp32(data: &[u8], start: usize, count: usize, entry: u32) -> Option<Image> {
    let ext = data.get(start + 8..start + 24)?;
    let chip_id = read_u16(ext, 4)?;
    let hash_appended = ext[15];
    if hash_appended > 1 {
        return None;
    }
    let mut chip = chip_from_id(chip_id)?;
    // ESP-IDF 5.0 added min_chip_rev_full (major * 100 + minor); before
    // that, byte 6 held the major revision and the rest was reserved (zero).
    let min_chip_rev = match read_u16(ext, 7)? {
        0 => ext[6] as u16 * 100,
        full => full,
    };
    let (segments, _) = read_segments(data, start + 24, count)?;
    if chip_id == 0 {
        // Either an ESP32 image or an ESP8266 RTOS SDK v3 image.
        let addrs: Vec<u32> = segments
            .iter()
            .filter(|s| s.addr != 0)
            .map(|s| s.addr)
            .collect();
        let windowed_entry = code_at(data, &segments, entry).map(xtensa::is_entry_insn);
        chip = guess_chip(&addrs, windowed_entry);
    }
    plausible(chip, entry, &segments).then_some(Image {
        chip,
        format: Format::Esp32,
        start,
        entry,
        segments,
        chip_id: Some(chip_id),
        min_chip_rev,
        hash_appended: hash_appended == 1,
    })
}

fn parse_esp8266_v1(data: &[u8], start: usize, count: usize, entry: u32) -> Option<Image> {
    let (segments, _) = read_segments(data, start + 8, count)?;
    plausible(Chip::Esp8266, entry, &segments).then_some(Image {
        chip: Chip::Esp8266,
        format: Format::Esp8266V1,
        start,
        entry,
        segments,
        chip_id: None,
        min_chip_rev: 0,
        hash_appended: false,
    })
}

fn parse_esp8266_v2(data: &[u8], start: usize, entry: u32) -> Option<Image> {
    // The irom0 segment: its header's address field is not meaningful.
    let irom_size = read_u32(data, start + 12)?;
    let irom_offset = start + 16;
    let second = irom_offset.checked_add(irom_size as usize)?;
    let header = data.get(second..second + 8)?;
    if header[0] != IMAGE_MAGIC || header[1] == 0 || header[1] > 16 {
        return None;
    }
    let (mut segments, _) = read_segments(data, second + 8, header[1] as usize)?;
    if !plausible(Chip::Esp8266, entry, &segments) {
        return None;
    }
    // The flash cache maps 1 MB windows of flash at 0x40200000; images are
    // flashed at the start of a window plus 0x1000 (OTA) or at 0.
    let irom_addr = ESP8266_FLASH_MAP
        + ((esp8266_v2_flash_offset(data, start, irom_offset, irom_size) + 0x10) & 0xF_FFFF);
    segments.insert(
        0,
        Segment {
            addr: irom_addr,
            offset: irom_offset,
            size: irom_size,
        },
    );
    Some(Image {
        chip: Chip::Esp8266,
        format: Format::Esp8266V2,
        start,
        entry,
        segments,
        chip_id: None,
        min_chip_rev: 0,
        hash_appended: false,
    })
}

/// The flash offset a v2 image was written to. A full flash dump says so
/// directly; a standalone image was written either at 0 or, for the OTA
/// layout's `user1.bin`, at 0x1000. The candidates are told apart by the
/// code pointers in the image's literal pools: with the right base they
/// point at function prologues (`addi a1, a1, -n`, the call0 ABI).
fn esp8266_v2_flash_offset(data: &[u8], start: usize, irom_offset: usize, irom_size: u32) -> u32 {
    if start != 0 {
        return start as u32;
    }
    let irom = &data[irom_offset..irom_offset + irom_size as usize];
    let score = |flash_offset: u32| {
        let base = ESP8266_FLASH_MAP + flash_offset + 0x10;
        irom.as_chunks::<4>()
            .0
            .iter()
            .map(|&w| u32::from_le_bytes(w))
            .filter(|&p| p >= base && p < base + irom_size && p % 4 == 0)
            .filter(|&p| {
                let o = (p - base) as usize;
                irom.get(o..o + 2) == Some(&[0x12, 0xc1])
            })
            .count()
    };
    if score(0x1000) > score(0) { 0x1000 } else { 0 }
}

/// A partition table entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub kind: u8,
    pub subtype: u8,
    pub offset: u32,
    pub size: u32,
    pub label: String,
}

pub const PARTITION_TABLE_OFFSET: usize = 0x8000;

/// Flash offsets a dump may start at: 0, or 0x1000 for ESP32/ESP32-S2
/// images that begin with the bootloader (as `esptool merge_bin` and many
/// firmware distributions produce them).
pub const DUMP_BASES: [usize; 2] = [0, 0x1000];

/// Reads the partition table of a dump that starts at flash offset `base`.
pub fn partitions(data: &[u8], base: usize) -> Vec<Partition> {
    let mut out = Vec::new();
    let table = PARTITION_TABLE_OFFSET - base;
    for off in (table..table + 0xC00).step_by(32) {
        let Some(entry) = data.get(off..off + 32) else {
            break;
        };
        if entry[0] != 0xAA || entry[1] != 0x50 {
            break;
        }
        let label = entry[12..28]
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as char)
            .collect();
        out.push(Partition {
            kind: entry[2],
            subtype: entry[3],
            offset: read_u32(entry, 4).unwrap(),
            size: read_u32(entry, 8).unwrap(),
            label,
        });
    }
    out
}

/// What was found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    pub image: Image,
    /// An ESP8266 flash dump without a partition table (NONOS SDK), whose
    /// first megabyte is mapped at the flash cache window `0x40200000`,
    /// where `irom0` code lives.
    pub map_flash_window: bool,
    /// The flash offset of the start of the file.
    pub flash_base: usize,
    /// The app partition the image was loaded from. Only that partition is
    /// loaded, not the rest of the dump.
    pub partition: Option<Partition>,
}

/// Which image of a flash dump to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection<'a> {
    /// The factory app, else the first OTA app that parses.
    Default,
    /// The app partition with this label.
    Partition(&'a str),
    /// The second stage bootloader.
    Bootloader,
}

/// The partition table of a dump and the flash offset the dump starts at.
pub fn partition_table(data: &[u8]) -> Option<(usize, Vec<Partition>)> {
    DUMP_BASES
        .iter()
        .map(|&base| (base, partitions(data, base)))
        .find(|(_, parts)| !parts.is_empty())
}

/// The app partitions of a dump: factory (0x00) first, then ota_0..ota_15
/// (0x10..0x1f) and test (0x20).
pub fn app_partitions(parts: &[Partition]) -> Vec<&Partition> {
    let mut apps: Vec<&Partition> = parts.iter().filter(|p| p.kind == 0).collect();
    apps.sort_by_key(|p| (p.subtype != 0, p.subtype));
    apps
}

/// Parses the image of an app partition, bounded by the partition: it may
/// not extend into whatever follows it in flash.
fn load_partition(data: &[u8], base: usize, p: &Partition) -> Option<Firmware> {
    let start = (p.offset as usize).checked_sub(base)?;
    let end = start.saturating_add(p.size as usize).min(data.len());
    let image = parse_image(data.get(..end)?, start)?;
    Some(Firmware {
        image,
        map_flash_window: false,
        flash_base: base,
        partition: Some(p.clone()),
    })
}

/// The second stage bootloader of a dump starting at flash offset `base`:
/// at 0x1000 on the ESP32 and ESP32-S2, at 0 on the ESP32-S3 and ESP8266. It
/// ends where the partition table starts.
fn load_bootloader(data: &[u8], base: usize) -> Option<Firmware> {
    let end = (PARTITION_TABLE_OFFSET - base).min(data.len());
    let data = data.get(..end)?;
    [0x1000usize.checked_sub(base), (base == 0).then_some(0)]
        .into_iter()
        .flatten()
        .find_map(|start| parse_image(data, start))
        .map(|image| Firmware {
            image,
            map_flash_window: false,
            flash_base: base,
            partition: None,
        })
}

/// Finds the image to load in a file: the selected image of a flash dump,
/// or by default either an image at offset 0, or the factory/OTA app of a
/// flash dump.
pub fn find_firmware(data: &[u8], selection: Selection) -> Option<Firmware> {
    match selection {
        Selection::Default => {}
        Selection::Partition(label) => {
            let (base, parts) = partition_table(data)?;
            let p = app_partitions(&parts)
                .into_iter()
                .find(|p| p.label == label)?;
            return load_partition(data, base, p);
        }
        Selection::Bootloader => {
            let base = partition_table(data).map_or(0, |(base, _)| base);
            return load_bootloader(data, base);
        }
    }
    if let Some((base, parts)) = partition_table(data)
        && let Some(f) = app_partitions(&parts)
            .into_iter()
            .find_map(|p| load_partition(data, base, p))
    {
        return Some(f);
    }
    // ESP8266 NONOS OTA layout: boot loader at 0, user1.bin (v2) at 0x1000.
    if data.get(0x1000) == Some(&IMAGE_V2_MAGIC)
        && let Some(image) = parse_image(data, 0x1000)
    {
        return Some(Firmware {
            image,
            map_flash_window: true,
            flash_base: 0,
            partition: None,
        });
    }
    if let Some(image) = parse_image(data, 0) {
        let map_flash_window = image.chip == Chip::Esp8266 && data.len() > 0x10000;
        return Some(Firmware {
            image,
            map_flash_window,
            flash_base: 0,
            partition: None,
        });
    }
    // ESP32/ESP32-S2 dumps start with the bootloader at 0x1000.
    parse_image(data, 0x1000).map(|image| Firmware {
        image,
        map_flash_window: false,
        flash_base: 0,
        partition: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(magic: u8, ext: Option<[u8; 16]>, entry: u32, segs: &[(u32, &[u8])]) -> Vec<u8> {
        let mut v = vec![magic, segs.len() as u8, 2, 0x20];
        v.extend(entry.to_le_bytes());
        if let Some(ext) = ext {
            v.extend(ext);
        }
        for (addr, data) in segs {
            v.extend(addr.to_le_bytes());
            v.extend((data.len() as u32).to_le_bytes());
            v.extend(*data);
        }
        v
    }

    fn ext(chip_id: u16) -> [u8; 16] {
        let mut e = [0u8; 16];
        e[0] = 0xEE;
        e[4..6].copy_from_slice(&chip_id.to_le_bytes());
        e
    }

    #[test]
    fn esp32_app() {
        let d = image(
            0xE9,
            Some(ext(0)),
            0x4008_0400,
            &[(0x3F40_0020, &[0; 32]), (0x4008_0000, &[0x36, 0x41, 0, 0])],
        );
        let i = parse_image(&d, 0).unwrap();
        assert_eq!(
            (i.chip, i.format, i.segments.len()),
            (Chip::Esp32, Format::Esp32, 2)
        );
    }

    #[test]
    fn old_esp32_app_without_chip_id() {
        // Pre chip_id images have zeros there; the entry is windowed code.
        let d = image(
            0xE9,
            Some([0; 16]),
            0x4008_0000,
            &[(0x4008_0000, &[0x36, 0x41, 0, 0]), (0x3FFB_0000, &[1; 8])],
        );
        assert_eq!(parse_image(&d, 0).unwrap().chip, Chip::Esp32);
    }

    #[test]
    fn minimum_chip_revision() {
        let seg: &[(u32, &[u8])] = &[(0x4008_0000, &[0x36, 0x41, 0, 0])];
        let rev = |e: [u8; 16]| {
            parse_image(&image(0xE9, Some(e), 0x4008_0000, seg), 0)
                .unwrap()
                .min_chip_rev
        };
        // Pre-chip_id and pre-revision images.
        assert_eq!(rev([0; 16]), 0);
        // ESP-IDF 4.x: major revision in byte 6.
        let mut e = ext(0);
        e[6] = 3;
        assert_eq!(rev(e), 300);
        // ESP-IDF 5.x: min_chip_rev_full (v3.1) takes precedence.
        e[7..9].copy_from_slice(&301u16.to_le_bytes());
        assert_eq!(rev(e), 301);
    }

    #[test]
    fn s2_and_s3() {
        let d = image(0xE9, Some(ext(2)), 0x4002_5000, &[(0x4002_5000, &[0; 4])]);
        assert_eq!(parse_image(&d, 0).unwrap().chip, Chip::Esp32S2);
        let d = image(
            0xE9,
            Some(ext(9)),
            0x4037_5000,
            &[(0x4037_5000, &[0; 4]), (0x3C00_0020, &[0; 4])],
        );
        assert_eq!(parse_image(&d, 0).unwrap().chip, Chip::Esp32S3);
        // RISC-V chips are not ours.
        let d = image(0xE9, Some(ext(5)), 0x4038_0000, &[(0x4038_0000, &[0; 4])]);
        assert!(parse_image(&d, 0).is_none());
    }

    #[test]
    fn esp8266_v1() {
        let d = image(
            0xE9,
            None,
            0x4010_0004,
            &[
                (0x4010_0000, &[0x12, 0xc1, 0xf0, 0]),
                (0x3FFE_8000, &[0; 16]),
            ],
        );
        let i = parse_image(&d, 0).unwrap();
        assert_eq!((i.chip, i.format), (Chip::Esp8266, Format::Esp8266V1));
    }

    #[test]
    fn esp32_call0_entry_stub() {
        let d = image(
            0xE9,
            Some(ext(0)),
            0x4008_0000,
            &[(0x4008_0000, &[0x3d, 0xf0, 0, 0])],
        );
        let i = parse_image(&d, 0).unwrap();
        assert_eq!((i.chip, i.format), (Chip::Esp32, Format::Esp32));
    }

    fn app_image(desc: &[u8], trailer: &[u8]) -> Vec<u8> {
        let mut d = image(
            0xE9,
            Some(ext(0)),
            0x4008_0000,
            &[
                (0x4008_0000, &[0x36, 0x41, 0x00, 0x1d, 0xf0]),
                (0x3F40_0020, desc),
            ],
        );
        d.extend(trailer);
        d
    }

    #[test]
    fn app_desc() {
        let mut desc = vec![0u8; APP_DESC_SIZE];
        desc[..4].copy_from_slice(&APP_DESC_MAGIC.to_le_bytes());
        desc[48..51].copy_from_slice(b"app");
        let d = app_image(&desc, &[]);
        let (addr, found) = parse_image(&d, 0).unwrap().app_desc(&d).unwrap();
        assert_eq!((addr, found), (0x3F40_0020, &desc[..]));

        // Truncated at the end of the file, or at the end of its segment
        // with other data following.
        let magic = APP_DESC_MAGIC.to_le_bytes();
        let d = app_image(&magic, &[]);
        assert_eq!(parse_image(&d, 0).unwrap().app_desc(&d), None);
        let d = app_image(&magic, &[0x41; 512]);
        assert_eq!(parse_image(&d, 0).unwrap().app_desc(&d), None);
    }

    #[test]
    fn esp8266_rtos_v3_with_extended_header() {
        let d = image(
            0xE9,
            Some(ext(0)),
            0x4010_0004,
            &[
                (0x4010_0000, &[0x12, 0xc1, 0xf0, 0, 0, 0, 0, 0]),
                (0x3FFE_8000, &[0; 16]),
            ],
        );
        let i = parse_image(&d, 0).unwrap();
        assert_eq!((i.chip, i.format), (Chip::Esp8266, Format::Esp32));
    }

    #[test]
    fn esp8266_v2() {
        let mut d = vec![0xEA, 4, 0, 0];
        d.extend(0x4010_0004u32.to_le_bytes());
        d.extend(0u32.to_le_bytes());
        d.extend(8u32.to_le_bytes());
        d.extend([0x12, 0xc1, 0xf0, 0, 0, 0, 0, 0]);
        d.extend(image(0xE9, None, 0x4010_0004, &[(0x4010_0000, &[0; 8])]));
        let i = parse_image(&d, 0).unwrap();
        assert_eq!(i.format, Format::Esp8266V2);
        assert_eq!(
            i.segments[0],
            Segment {
                addr: 0x4020_0010,
                offset: 16,
                size: 8
            }
        );
    }

    #[test]
    fn rejects_x86_jump() {
        let mut d = vec![0xE9, 0x10, 0x00, 0x00, 0x00, 0x90, 0x90];
        d.resize(64, 0);
        assert!(parse_image(&d, 0).is_none());
    }

    /// A dump starting at flash offset 0 with these `(label, subtype,
    /// offset, size)` app partitions; the returned dump ends at 0x10000.
    fn dump(parts: &[(&str, u8, u32, u32)]) -> Vec<u8> {
        let mut d = vec![0xFF; 0x10000];
        for (i, &(label, subtype, offset, size)) in parts.iter().enumerate() {
            let mut entry = vec![0xAA, 0x50, 0x00, subtype];
            entry.extend(offset.to_le_bytes());
            entry.extend(size.to_le_bytes());
            let mut name = [0u8; 16];
            name[..label.len()].copy_from_slice(label.as_bytes());
            entry.extend(name);
            entry.extend([0; 4]);
            let at = PARTITION_TABLE_OFFSET + 32 * i;
            d[at..at + 32].copy_from_slice(&entry);
        }
        d
    }

    fn s3_app(entry: u32) -> Vec<u8> {
        image(0xE9, Some(ext(9)), entry, &[(0x4037_5000, &[0; 16])])
    }

    #[test]
    fn flash_dump_with_partition_table() {
        let mut d = dump(&[("factory", 0, 0x10000, 0x100000)]);
        d.extend(s3_app(0x4037_5000));
        let f = find_firmware(&d, Selection::Default).unwrap();
        assert!(!f.map_flash_window);
        assert_eq!(f.image.start, 0x10000);
        assert_eq!(f.partition.unwrap().label, "factory");
    }

    #[test]
    fn image_is_bounded_by_its_partition() {
        // The image's segment runs past the end of the 0x20-byte partition.
        let mut d = dump(&[("factory", 0, 0x10000, 0x20)]);
        d.extend(s3_app(0x4037_5000));
        assert!(find_firmware(&d, Selection::Default).is_none_or(|f| f.partition.is_none()));
        assert_eq!(find_firmware(&d, Selection::Partition("factory")), None);
    }

    #[test]
    fn selected_partition() {
        let mut d = dump(&[
            ("ota_0", 0x10, 0x10000, 0x1000),
            ("ota_1", 0x11, 0x11000, 0x1000),
        ]);
        d.extend(s3_app(0x4037_5000));
        d.resize(0x11000, 0xFF);
        d.extend(s3_app(0x4037_5004));
        let entry = |sel| find_firmware(&d, sel).map(|f| f.image.entry);
        assert_eq!(entry(Selection::Default), Some(0x4037_5000));
        assert_eq!(entry(Selection::Partition("ota_0")), Some(0x4037_5000));
        assert_eq!(entry(Selection::Partition("ota_1")), Some(0x4037_5004));
        assert_eq!(entry(Selection::Partition("nvs")), None);
        assert_eq!(entry(Selection::Bootloader), None);
    }

    #[test]
    fn selected_bootloader() {
        let mut d = dump(&[("factory", 0, 0x10000, 0x100000)]);
        let boot = s3_app(0x4037_5008);
        d[..boot.len()].copy_from_slice(&boot);
        d.extend(s3_app(0x4037_5000));
        let f = find_firmware(&d, Selection::Bootloader).unwrap();
        assert_eq!((f.image.start, f.image.entry), (0, 0x4037_5008));
        assert_eq!(f.partition, None);
    }
}
