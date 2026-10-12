//! Espressif firmware image formats and memory maps.
//!
//! Supported layouts:
//!
//! * **ESP32-family images** (ESP32, ESP32-S2, ESP32-S3 and the RISC-V
//!   ESP32-C2, C3, C6, H2 and C5; all ESP-IDF versions, apps and second
//!   stage bootloaders): an 8-byte header with magic `0xE9`, a 16-byte
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
//! * **Flash dumps**: dumps with a partition table at 0x8000 are split into
//!   their bootloader, partition table and partitions by the `EspFlash`
//!   container transform (see [`crate::container`]). Where the
//!   view is given the whole dump instead, only one image of it is loaded:
//!   the factory (or first OTA) application, or the app partition or
//!   bootloader the `files.espressif.partition` setting selects. ESP8266
//!   dumps without a partition table (NONOS SDK) are additionally mapped as
//!   a whole at the flash cache window `0x40200000`, where `irom0` lives.

use std::collections::HashSet;

use crate::{Chip, EntryHint, Family};

pub const IMAGE_MAGIC: u8 = 0xE9;
pub const IMAGE_V2_MAGIC: u8 = 0xEA;
pub const APP_DESC_MAGIC: u32 = 0xABCD_5432;
/// `sizeof(esp_app_desc_t)`.
pub const APP_DESC_SIZE: usize = 256;
pub const ESP8266_FLASH_MAP: u32 = 0x4020_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Rom,
    /// The mask ROM seen from the data bus (the Xtensa chips, C2, C3): its
    /// constants.
    RomData,
    Iram,
    Dram,
    /// Internal SRAM at the same address on the instruction and data buses
    /// (ESP32-C6, H2, C5), holding both code and data.
    Sram,
    /// Flash mapped, executable.
    Irom,
    /// Flash mapped, read-only data.
    Drom,
    /// Flash mapped through a cache window shared by code and read-only
    /// data (ESP32-C6, H2, C5).
    Flash,
    RtcCode,
    RtcData,
    /// RTC (or LP) memory at the same address for code and data, holding
    /// `.rtc.text` and the retained `.rtc.data`, `.rtc.bss` and
    /// `.rtc_noinit` (ESP-IDF aliases `rtc_data_seg` to `rtc_iram_seg`, or
    /// both to `lp_ram_seg`).
    RtcRam,
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
            RegionKind::Iram
                | RegionKind::Irom
                | RegionKind::RtcCode
                | RegionKind::RtcRam
                | RegionKind::Rom
                | RegionKind::Sram
                | RegionKind::Flash
        )
    }

    /// Does the region hold the read-only data of an image (where its
    /// `esp_app_desc_t` is)?
    pub fn is_rodata(&self) -> bool {
        matches!(self.kind, RegionKind::Drom | RegionKind::Flash)
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
// SDK's ld scripts. soc.h leaves out the ROM's data bus alias (`rom_data`):
// that is where Espressif's ROM ELF files load the ROM's .rodata (their
// virtual addresses; the physical ones are in the ROM), and what probe-rs's
// target descriptions call "ROM1, Data bus".
pub static ESP8266_REGIONS: &[Region] = &[
    r("dport", 0x3FF0_0000, 0x3FF0_1000, K::Peripheral),
    r("dram", 0x3FFE_8000, 0x4000_0000, K::Dram),
    r("rom", 0x4000_0000, 0x4001_0000, K::Rom),
    r("iram", 0x4010_0000, 0x4011_0000, K::Iram),
    r("irom", 0x4020_0000, 0x4040_0000, K::Irom),
    r("peripherals", 0x6000_0000, 0x6000_1000, K::Peripheral),
    r("rtc_data", 0x6000_1000, 0x6000_2000, K::RtcData),
];

pub static ESP32_REGIONS: &[Region] = &[
    r("drom", 0x3F40_0000, 0x3F80_0000, K::Drom),
    r("extram", 0x3F80_0000, 0x3FC0_0000, K::ExtRam),
    r("peripherals", 0x3FF0_0000, 0x3FF8_0000, K::Peripheral),
    r("rtc_dram", 0x3FF8_0000, 0x3FF8_2000, K::RtcData),
    // ROM1 (0x4006_0000..0x4007_0000) as the data bus sees it: the ROM's
    // .rodata.
    r("rom_data", 0x3FF9_0000, 0x3FFA_0000, K::RomData),
    r("dram", 0x3FFA_E000, 0x4000_0000, K::Dram),
    r("rom", 0x4000_0000, 0x4007_0000, K::Rom),
    r("iram", 0x4007_0000, 0x400C_0000, K::Iram),
    r("rtc_iram", 0x400C_0000, 0x400C_2000, K::RtcCode),
    r("irom", 0x400C_2000, 0x4040_0000, K::Irom),
    r("rtc_slow", 0x5000_0000, 0x5000_2000, K::RtcData),
];

pub static ESP32S2_REGIONS: &[Region] = &[
    r("drom", 0x3F00_0000, 0x3F40_0000, K::Drom),
    r("peripherals", 0x3F40_0000, 0x3F50_0000, K::Peripheral),
    r("extram", 0x3F50_0000, 0x3FF8_0000, K::ExtRam),
    r("rtc_dram", 0x3FF9_E000, 0x3FFA_0000, K::RtcData),
    // ROM1 (0x4001_0000..0x4002_0000) as the data bus sees it: the ROM's
    // .rodata.
    r("rom_data", 0x3FFA_0000, 0x3FFB_0000, K::RomData),
    r("dram", 0x3FFB_0000, 0x4000_0000, K::Dram),
    r("rom", 0x4000_0000, 0x4002_0000, K::Rom),
    r("iram", 0x4002_0000, 0x4007_0000, K::Iram),
    r("rtc_iram", 0x4007_0000, 0x4007_2000, K::RtcCode),
    r("irom", 0x4008_0000, 0x4080_0000, K::Irom),
    r("rtc_slow", 0x5000_0000, 0x5000_2000, K::RtcData),
];

pub static ESP32S3_REGIONS: &[Region] = &[
    r("drom", 0x3C00_0000, 0x3E00_0000, K::Drom),
    r("dram", 0x3FC8_0000, 0x3FD0_0000, K::Dram),
    // The upper 128 KiB of the ROM (ROM1, 0x4004_0000..0x4006_0000) as the
    // data bus sees it: the ROM's .rodata.
    r("rom_data", 0x3FF0_0000, 0x3FF2_0000, K::RomData),
    r("rom", 0x4000_0000, 0x4006_0000, K::Rom),
    r("iram", 0x4037_0000, 0x403E_0000, K::Iram),
    r("irom", 0x4200_0000, 0x4400_0000, K::Irom),
    r("rtc_slow", 0x5000_0000, 0x5000_2000, K::RtcData),
    r("peripherals", 0x6000_0000, 0x600D_1000, K::Peripheral),
    r("rtc_fast", 0x600F_E000, 0x6010_0000, K::RtcRam),
];

// The RISC-V chips. The ESP32-C2 and C3 have separate instruction and data
// buses like the ESP32-S3: IRAM and DRAM are the same SRAM at two addresses
// (`SOC_I_D_OFFSET` apart), and flash is mapped through separate code and
// data windows. The C6, H2 and C5 see SRAM at one address from both buses
// and map flash through one window shared by code and read-only data. The
// end of each flash window is the largest the MMU can map; the SRAM regions
// include the top of SRAM that the ROM keeps for its own data.

pub static ESP32C2_REGIONS: &[Region] = &[
    r("drom", 0x3C00_0000, 0x3C40_0000, K::Drom),
    r("dram", 0x3FCA_0000, 0x3FCE_0000, K::Dram),
    r("rom_data", 0x3FF0_0000, 0x3FF5_0000, K::RomData),
    r("rom", 0x4000_0000, 0x4009_0000, K::Rom),
    r("iram", 0x4037_C000, 0x403C_0000, K::Iram),
    r("irom", 0x4200_0000, 0x4240_0000, K::Irom),
    r("peripherals", 0x6000_0000, 0x6010_0000, K::Peripheral),
];

pub static ESP32C3_REGIONS: &[Region] = &[
    r("drom", 0x3C00_0000, 0x3C80_0000, K::Drom),
    r("dram", 0x3FC8_0000, 0x3FCE_0000, K::Dram),
    r("rom_data", 0x3FF0_0000, 0x3FF2_0000, K::RomData),
    r("rom", 0x4000_0000, 0x4006_0000, K::Rom),
    r("iram", 0x4037_C000, 0x403E_0000, K::Iram),
    r("irom", 0x4200_0000, 0x4280_0000, K::Irom),
    r("rtc_fast", 0x5000_0000, 0x5000_2000, K::RtcRam),
    r("peripherals", 0x6000_0000, 0x6010_0000, K::Peripheral),
];

pub static ESP32C6_REGIONS: &[Region] = &[
    r("rom", 0x4000_0000, 0x4005_0000, K::Rom),
    r("sram", 0x4080_0000, 0x4088_0000, K::Sram),
    r("flash", 0x4200_0000, 0x4300_0000, K::Flash),
    r("lp_ram", 0x5000_0000, 0x5000_4000, K::RtcRam),
    r("peripherals", 0x6000_0000, 0x6010_0000, K::Peripheral),
];

pub static ESP32H2_REGIONS: &[Region] = &[
    r("rom", 0x4000_0000, 0x4002_0000, K::Rom),
    r("sram", 0x4080_0000, 0x4085_0000, K::Sram),
    r("flash", 0x4200_0000, 0x4300_0000, K::Flash),
    r("lp_ram", 0x5000_0000, 0x5000_1000, K::RtcRam),
    r("peripherals", 0x6000_0000, 0x6010_0000, K::Peripheral),
];

// The C5's flash window also maps PSRAM.
pub static ESP32C5_REGIONS: &[Region] = &[
    r("rom", 0x4000_0000, 0x4005_0000, K::Rom),
    r("sram", 0x4080_0000, 0x4086_0000, K::Sram),
    r("flash", 0x4200_0000, 0x4400_0000, K::Flash),
    r("lp_ram", 0x5000_0000, 0x5000_4000, K::RtcRam),
    r("peripherals", 0x6000_0000, 0x6010_0000, K::Peripheral),
];

pub fn region_of(chip: Chip, addr: u32) -> Option<&'static Region> {
    chip.regions().iter().find(|r| r.contains(addr))
}

/// The chip of an ESP-IDF image `chip_id`. Other ids belong to chips that
/// are not supported (see [`UNSUPPORTED_CHIPS`]).
pub fn chip_from_id(id: u16) -> Option<Chip> {
    Chip::ALL
        .into_iter()
        .find(|c| c.image_chip_id() == Some(id))
}

/// Chips with an ESP-IDF image `chip_id` (`esp_chip_id_t` in ESP-IDF's
/// `esp_app_format.h`) that are not supported. The ESP32-P4's RISC-V cores
/// have custom instructions that are not decoded.
pub const UNSUPPORTED_CHIPS: &[(u16, &str)] = &[
    (18, "ESP32-P4"),
    (20, "ESP32-C61"),
    (25, "ESP32-H21"),
    (28, "ESP32-H4"),
    (32, "ESP32-S31"),
];

/// The name of the chip of an ESP-IDF image `chip_id`, supported or not.
pub fn chip_name_from_id(id: u16) -> Option<&'static str> {
    chip_from_id(id).map(Chip::display_name).or_else(|| {
        UNSUPPORTED_CHIPS
            .iter()
            .find(|&&(chip_id, _)| chip_id == id)
            .map(|&(_, name)| name)
    })
}

/// Guesses the chip from the load addresses of a program, for formats that
/// do not say (ELF files, and images with a `chip_id` of 0, which is the
/// ESP32 or an ESP8266 RTOS SDK v3 image). `hint` is what the code at the
/// entry point says, if known: ESP32-family code uses the windowed ABI,
/// whose functions start with `entry`, while the ESP8266 uses `call0`.
pub fn guess_chip(family: Family, addrs: &[u32], hint: Option<EntryHint>) -> Chip {
    match family {
        Family::Xtensa => guess_xtensa_chip(addrs, hint),
        Family::RiscV => guess_riscv_chip(addrs),
    }
}

/// Does every `(address, size)` range of `segments` lie in `chip`'s memory
/// map? Each range must be covered entirely, by one region or by adjacent
/// ones; an empty range only needs its address mapped.
pub fn fits_memory_map(chip: Chip, segments: &[(u32, u32)]) -> bool {
    segments.iter().all(|&(addr, size)| {
        let last = addr.wrapping_add(size.max(1) - 1);
        if last < addr {
            return false;
        }
        let mut cursor = addr;
        loop {
            let Some(region) = region_of(chip, cursor) else {
                return false;
            };
            // Region ends are exclusive.
            if last < region.end {
                return true;
            }
            cursor = region.end;
        }
    })
}

/// The ROM symbols (name and address) of `chip`, of every chip revision.
/// A symbol listed by several revisions appears once.
pub fn rom_symbol_set(chip: Chip) -> HashSet<(&'static str, u32)> {
    let (base, revisions) = chip.rom_symbols();
    std::iter::once(base)
        .chain(revisions.iter().map(|&(_, table)| table))
        .flatten()
        .copied()
        .collect()
}

/// How many of a program's `ABS` symbols (name and address) are ROM
/// symbols of `chip`, of any chip revision. ESP-IDF ELF files carry one for
/// each ROM function or variable they use. Each of the program's symbols
/// counts once, however many of the chip's tables list it.
pub fn rom_matches(chip: Chip, symbols: &HashSet<(&str, u32)>) -> usize {
    let rom = rom_symbol_set(chip);
    symbols.iter().filter(|s| rom.contains(*s)).count()
}

/// Guesses the chip of an ELF file of `family`: among the family's chips
/// whose memory map holds every loaded segment, the one whose ROM symbols
/// the file's `ABS` symbols match best, if any match. Otherwise (no match,
/// or a tie) the load addresses decide, as in [`guess_chip`]. Returns the
/// chip and the number of matching ROM symbols.
pub fn guess_elf_chip(
    family: Family,
    segments: &[(u32, u32)],
    hint: Option<EntryHint>,
    abs_symbols: &[(String, u32)],
) -> (Chip, usize) {
    let addrs: Vec<u32> = segments.iter().map(|&(addr, _)| addr).collect();
    let fallback = guess_chip(family, &addrs, hint);
    let family_chips = || Chip::ALL.into_iter().filter(|c| c.cpu().family() == family);
    let mut candidates: Vec<Chip> = family_chips()
        .filter(|&c| fits_memory_map(c, segments))
        .collect();
    if candidates.is_empty() {
        candidates = family_chips().collect();
    }
    let symbols: HashSet<(&str, u32)> = abs_symbols
        .iter()
        .map(|(name, addr)| (name.as_str(), *addr))
        .collect();
    let scores: Vec<(Chip, usize)> = candidates
        .into_iter()
        .map(|c| (c, rom_matches(c, &symbols)))
        .collect();
    let best = scores.iter().map(|&(_, n)| n).max().unwrap_or(0);
    let leaders: Vec<Chip> = scores
        .iter()
        .filter(|&&(_, n)| n == best)
        .map(|&(c, _)| c)
        .collect();
    // With no ROM symbols matching, every candidate leads. The address
    // heuristic only breaks ties among chips whose memory map holds the
    // program: it guesses from where segments start, and may name a chip
    // the segments run off the end of. Otherwise the first in `Chip::ALL`
    // order, the older and more common chip, is taken.
    match leaders.as_slice() {
        [chip] => (*chip, best),
        tied if tied.contains(&fallback) => (fallback, best),
        tied => (tied[0], best),
    }
}

/// The RISC-V chips' memory maps mostly overlap, so this is a best guess:
/// the C2's map is contained in the C3's, and the H2's and the C5's SRAM in
/// the C6's. Ambiguous programs are taken for the more common C3 or C6.
fn guess_riscv_chip(addrs: &[u32]) -> Chip {
    let any_in = |ranges: &[(u32, u32)]| {
        addrs
            .iter()
            .any(|&a| ranges.iter().any(|&(lo, hi)| lo <= a && a < hi))
    };
    // Only the C5's flash window extends past 16 MB.
    if any_in(&[(0x4300_0000, 0x4400_0000)]) {
        return Chip::Esp32C5;
    }
    // Unified SRAM: the C6, H2 or C5. Only the C6 has SRAM above 0x40860000,
    // but programs below that are not told apart.
    if any_in(&[(0x4080_0000, 0x4090_0000)]) {
        return Chip::Esp32C6;
    }
    // Separate instruction and data buses: the C3 or the C2, whose memory
    // is a subset of the C3's.
    let c3_only = [
        (0x3C40_0000, 0x3C80_0000),
        (0x3FC8_0000, 0x3FCA_0000),
        (0x403C_0000, 0x403E_0000),
        (0x4240_0000, 0x4280_0000),
        (0x5000_0000, 0x5000_2000),
    ];
    let c2_only = [(0x3FF2_0000, 0x3FF5_0000), (0x4006_0000, 0x4009_0000)];
    if !any_in(&c3_only) && any_in(&c2_only) {
        return Chip::Esp32C2;
    }
    Chip::Esp32C3
}

fn guess_xtensa_chip(addrs: &[u32], hint: Option<EntryHint>) -> Chip {
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
    match hint {
        Some(EntryHint::Windowed) => Chip::Esp32,
        Some(EntryHint::Call0) if fits_esp8266 => Chip::Esp8266,
        Some(EntryHint::Call0) => Chip::Esp32,
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
            .filter(|s| region_of(self.chip, s.addr).is_some_and(|r| r.is_rodata()))
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
    let min_chip_rev_full = read_u16(ext, 7)?;
    let (segments, _) = read_segments(data, start + 24, count)?;
    if chip_id == 0 {
        // Either an ESP32 image or an ESP8266 RTOS SDK v3 image.
        let addrs: Vec<u32> = segments
            .iter()
            .filter(|s| s.addr != 0)
            .map(|s| s.addr)
            .collect();
        let family = Family::Xtensa;
        let hint = code_at(data, &segments, entry).and_then(|code| family.entry_hint(code));
        chip = guess_chip(family, &addrs, hint);
    }
    // ESP-IDF 5.0 added min_chip_rev_full (major * 100 + minor); before
    // that, byte 6 held the revision (see Chip::legacy_chip_rev) and the
    // rest was reserved (zero).
    let min_chip_rev = match min_chip_rev_full {
        0 => chip.legacy_chip_rev(ext[6]),
        full => full,
    };
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

/// Where the flash cache maps the `irom0` segment of an ESP8266 v2 image
/// written at `flash_offset`: 1 MB windows of flash at 0x40200000, and the
/// segment's data starts 0x10 bytes into the image.
fn esp8266_v2_irom_addr(flash_offset: u32) -> u32 {
    ESP8266_FLASH_MAP + (flash_offset.wrapping_add(0x10) & 0xF_FFFF)
}

impl Image {
    /// Places the image at `flash_offset`, where it is known (a file
    /// extracted from a flash dump), in place of where its position in the
    /// file says or [`esp8266_v2_flash_offset`] guesses. Only an ESP8266 v2
    /// image's `irom0` segment depends on it.
    pub fn set_flash_offset(&mut self, flash_offset: u32) {
        if self.format == Format::Esp8266V2
            && let Some(irom) = self.segments.first_mut()
        {
            irom.addr = esp8266_v2_irom_addr(flash_offset.wrapping_add(self.start as u32));
        }
    }
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
    let irom_addr =
        esp8266_v2_irom_addr(esp8266_v2_flash_offset(data, start, irom_offset, irom_size));
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
                irom.get(o..)
                    .is_some_and(|code| Chip::Esp8266.cpu().is_prologue(code))
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

/// The size of the partition table: at most 95 entries and an MD5 entry,
/// in a 4 KiB flash sector of its own.
pub const PARTITION_TABLE_SIZE: usize = 0xC00;

/// Reads the partition table of a dump that starts at flash offset `base`.
///
/// The table must look like one `gen_esp32part.py` writes, so that other
/// files with the magic at that offset are not taken for flash dumps: at
/// least one entry, each of a known type (or a custom one, 0x40 and up), at
/// a 4 KiB-aligned offset after the table, of a non-zero size, and with a
/// printable label; and after the entries, the MD5 entry, erased flash or
/// the end of the data.
pub fn partitions(data: &[u8], base: usize) -> Option<Vec<Partition>> {
    parse_partition_table(data.get(PARTITION_TABLE_OFFSET.checked_sub(base)?..)?)
}

/// Parses the partition table at the start of `table` (see [`partitions`]).
pub fn parse_partition_table(table: &[u8]) -> Option<Vec<Partition>> {
    let mut out = Vec::new();
    for entry in table
        .as_chunks::<32>()
        .0
        .iter()
        .take(PARTITION_TABLE_SIZE / 32)
    {
        if entry[0] != 0xAA || entry[1] != 0x50 {
            let md5 = entry[..2] == [0xEB, 0xEB] && entry[2..16].iter().all(|&b| b == 0xFF);
            if !md5 && entry.iter().any(|&b| b != 0xFF) {
                return None;
            }
            break;
        }
        let p = Partition {
            kind: entry[2],
            subtype: entry[3],
            offset: read_u32(entry, 4)?,
            size: read_u32(entry, 8)?,
            label: entry[12..28]
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as char)
                .collect(),
        };
        let known_type = matches!(p.kind, 0..=3 | 0x40..=0xFE);
        let placed = (p.offset as usize).is_multiple_of(0x1000)
            && p.offset as usize >= PARTITION_TABLE_OFFSET + 0x1000
            && p.size != 0
            && p.offset.checked_add(p.size - 1).is_some();
        let label = !p.label.is_empty() && p.label.chars().all(|c| (' '..='~').contains(&c));
        if !(known_type && placed && label) {
            return None;
        }
        out.push(p);
    }
    (!out.is_empty()).then_some(out)
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
        .find_map(|&base| Some((base, partitions(data, base)?)))
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

/// Is `data`, which starts with the ESP8266 `image`, a NONOS SDK flash dump,
/// whose `irom0` code (flashed separately, at 0x10000 or 0x40000) is only
/// reached through the flash cache window? Its image is a plain v1 image
/// that maps no flash itself, and something is written past it, at 0x10000
/// or later. An app on its own (an RTOS SDK app partition read out of
/// flash, however long, padded with erased flash) is not.
fn is_nonos_dump(data: &[u8], image: &Image) -> bool {
    let maps_flash = image
        .segments
        .iter()
        .any(|s| region_of(Chip::Esp8266, s.addr).is_some_and(|r| r.kind == K::Irom));
    let end = image
        .segments
        .iter()
        .map(|s| s.offset + s.size as usize)
        .max()
        .unwrap_or(0);
    image.chip == Chip::Esp8266
        && image.chip_id.is_none()
        && !maps_flash
        && data
            .get(end.max(0x10000)..)
            .is_some_and(|rest| rest.iter().any(|&b| b != 0xFF))
}

/// An ESP32-family image header, at file offset `start`, for a chip that is
/// not supported: its `chip_id` is that of no supported chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedImage {
    pub start: usize,
    pub chip_id: u16,
}

impl UnsupportedImage {
    /// What to tell the user about the image.
    pub fn message(&self) -> String {
        let id = self.chip_id;
        match chip_name_from_id(id) {
            Some(name) => format!(
                "ESP image for {name} (chip id {id}) at file offset {:#x} is not supported by binja-espressif",
                self.start
            ),
            None => format!(
                "ESP image with unknown chip id {id} ({id:#x}) at file offset {:#x} is not supported by binja-espressif",
                self.start
            ),
        }
    }
}

/// The header at `start`, if it is an ESP32-family image header (magic,
/// segment count, the extended header's reserved bytes and hash flag, and
/// segment headers that fit in the file) whose `chip_id` no supported chip
/// has.
fn unsupported_image_at(data: &[u8], start: usize) -> Option<UnsupportedImage> {
    let header = data.get(start..start.checked_add(24)?)?;
    let count = header[1] as usize;
    let ext = &header[8..];
    let chip_id = read_u16(ext, 4)?;
    // The extended header starts with the WP pin (a GPIO number, or 0xEE
    // for none) and three bytes of SPI pin drive strengths, which esptool
    // and ESP-IDF leave at 0. Where an ESP8266 v1 image, which has no
    // extended header, has the load address of its first segment: never
    // below 0x100.
    let plausible = header[0] == IMAGE_MAGIC
        && (1..=16).contains(&count)
        && read_u32(ext, 0)? <= 0xFF
        && ext[11..15] == [0; 4]
        && ext[15] <= 1;
    if !plausible || chip_from_id(chip_id).is_some() || parse_image(data, start).is_some() {
        return None;
    }
    read_segments(data, start + 24, count)?;
    Some(UnsupportedImage { start, chip_id })
}

/// Finds an image of an unsupported chip where [`find_firmware`] looks for
/// the `selection`: by default in the app partitions of a flash dump, at
/// offset 0 and at 0x1000. For saying why a file with no loadable image is
/// not loaded.
pub fn find_unsupported(data: &[u8], selection: Selection) -> Option<UnsupportedImage> {
    let table = partition_table(data);
    let in_partition = |p: &Partition| {
        let base = table.as_ref()?.0;
        let start = (p.offset as usize).checked_sub(base)?;
        let end = start.saturating_add(p.size as usize).min(data.len());
        unsupported_image_at(data.get(..end)?, start)
    };
    let apps = table
        .as_ref()
        .map(|(_, parts)| app_partitions(parts))
        .unwrap_or_default();
    match selection {
        Selection::Default => apps.into_iter().find_map(in_partition).or_else(|| {
            [0, 0x1000]
                .into_iter()
                .find_map(|start| unsupported_image_at(data, start))
        }),
        Selection::Partition(label) => apps
            .into_iter()
            .find(|p| p.label == label)
            .and_then(in_partition),
        Selection::Bootloader => {
            let base = table.as_ref().map_or(0, |&(base, _)| base);
            let end = (PARTITION_TABLE_OFFSET - base).min(data.len());
            [0x1000usize.checked_sub(base), (base == 0).then_some(0)]
                .into_iter()
                .flatten()
                .find_map(|start| unsupported_image_at(&data[..end], start))
        }
    }
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
        let map_flash_window = is_nonos_dump(data, &image);
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

        // On the ESP32-C3 the legacy byte is the minor revision: 3 is v0.3,
        // not v3.0.
        let c3_seg: &[(u32, &[u8])] = &[(0x4038_0000, &[0x13, 0, 0, 0])];
        let c3_rev = |e: [u8; 16]| {
            parse_image(&image(0xE9, Some(e), 0x4038_0000, c3_seg), 0)
                .unwrap()
                .min_chip_rev
        };
        let mut e = ext(5);
        e[6] = 3;
        assert_eq!(c3_rev(e), 3);
        e[7..9].copy_from_slice(&101u16.to_le_bytes());
        assert_eq!(c3_rev(e), 101);
        // The S3 (chip_id 9): a major revision, like the ESP32.
        assert_eq!(Chip::Esp32S3.legacy_chip_rev(1), 100);
        assert_eq!(Chip::Esp32C3.legacy_chip_rev(4), 4);
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
        // The ESP32-P4 is not supported.
        let d = image(0xE9, Some(ext(18)), 0x4FF0_0000, &[(0x4FF0_0000, &[0; 4])]);
        assert!(parse_image(&d, 0).is_none());
    }

    #[test]
    fn chip_names() {
        // ESP-IDF's esp_chip_id_t (esp_app_format.h) and espflash's
        // Chip::id().
        for (id, name) in [
            (0, "ESP32"),
            (2, "ESP32-S2"),
            (5, "ESP32-C3"),
            (9, "ESP32-S3"),
            (12, "ESP32-C2"),
            (13, "ESP32-C6"),
            (16, "ESP32-H2"),
            (18, "ESP32-P4"),
            (20, "ESP32-C61"),
            (23, "ESP32-C5"),
            (25, "ESP32-H21"),
            (28, "ESP32-H4"),
            (32, "ESP32-S31"),
        ] {
            assert_eq!(chip_name_from_id(id), Some(name), "{id}");
        }
        assert_eq!(chip_name_from_id(0x7F), None);
        assert_eq!(chip_name_from_id(0xFFFF), None);
        // No chip is both supported and unsupported.
        for &(id, name) in UNSUPPORTED_CHIPS {
            assert_eq!(chip_from_id(id), None, "{name}");
        }
    }

    #[test]
    fn unsupported_chips() {
        let seg: &[(u32, &[u8])] = &[(0x4FF0_0000, &[0; 4])];
        for (id, message) in [
            (
                20,
                "ESP image for ESP32-C61 (chip id 20) at file offset 0x0 is not supported by binja-espressif",
            ),
            (
                18,
                "ESP image for ESP32-P4 (chip id 18) at file offset 0x0 is not supported by binja-espressif",
            ),
            (
                0x7F,
                "ESP image with unknown chip id 127 (0x7f) at file offset 0x0 is not supported by binja-espressif",
            ),
        ] {
            let d = image(0xE9, Some(ext(id)), 0x4FF0_0000, seg);
            assert!(find_firmware(&d, Selection::Default).is_none());
            let found = find_unsupported(&d, Selection::Default).unwrap();
            assert_eq!(
                found,
                UnsupportedImage {
                    start: 0,
                    chip_id: id
                }
            );
            assert_eq!(found.message(), message);
        }
        // At 0x1000, after an ESP32/S2 dump's empty first sector.
        let mut d = vec![0xFF; 0x1000];
        d.extend(image(0xE9, Some(ext(20)), 0x4080_0000, seg));
        assert_eq!(
            find_unsupported(&d, Selection::Default),
            Some(UnsupportedImage {
                start: 0x1000,
                chip_id: 20
            })
        );
        // Supported chips, ESP8266 images and other data are not reported.
        let d = image(0xE9, Some(ext(13)), 0x4FF0_0000, seg);
        assert_eq!(find_unsupported(&d, Selection::Default), None);
        let d = image(0xE9, None, 0x4010_0000, &[(0x4010_0000, &[0; 32])]);
        assert!(parse_image(&d, 0).is_some());
        assert_eq!(find_unsupported(&d, Selection::Default), None);
        let mut e = ext(20);
        e[12] = 1;
        assert_eq!(
            find_unsupported(&image(0xE9, Some(e), 0, seg), Selection::Default),
            None
        );
        // An ESP8266 v1 image that does not parse (its segment is outside
        // the memory map) is not taken for an ESP32-family image whose
        // chip_id is its segment size.
        let d = image(0xE9, None, 0x4010_0000, &[(0x4100_0000, &[0; 0x1000])]);
        assert!(parse_image(&d, 0).is_none());
        assert_eq!(find_unsupported(&d, Selection::Default), None);
        // Segments past the end of the file.
        let mut d = image(0xE9, Some(ext(20)), 0x4080_0000, seg);
        d.truncate(30);
        assert_eq!(find_unsupported(&d, Selection::Default), None);
        assert_eq!(find_unsupported(&[0xE9; 8], Selection::Default), None);
    }

    #[test]
    fn riscv_chips() {
        // addi sp, sp, -16; ret
        let code: &[u8] = &[0x13, 0x01, 0x01, 0xff, 0x82, 0x80];
        let d = image(
            0xE9,
            Some(ext(5)),
            0x4038_0000,
            &[(0x3C00_0020, &[0; 32]), (0x4038_0000, code)],
        );
        assert_eq!(parse_image(&d, 0).unwrap().chip, Chip::Esp32C3);
        for (id, chip) in [
            (13, Chip::Esp32C6),
            (16, Chip::Esp32H2),
            (23, Chip::Esp32C5),
        ] {
            let d = image(
                0xE9,
                Some(ext(id)),
                0x4080_0000,
                &[(0x4200_0020, &[0; 32]), (0x4080_0000, code)],
            );
            assert_eq!(parse_image(&d, 0).unwrap().chip, chip);
        }
        let d = image(0xE9, Some(ext(12)), 0x4038_0000, &[(0x4038_0000, code)]);
        assert_eq!(parse_image(&d, 0).unwrap().chip, Chip::Esp32C2);
        // An entry point outside the chip's code regions.
        let d = image(0xE9, Some(ext(13)), 0x4038_0000, &[(0x4038_0000, code)]);
        assert!(parse_image(&d, 0).is_none());
    }

    #[test]
    fn chip_ids_and_regions() {
        for chip in Chip::ALL {
            if let Some(id) = chip.image_chip_id() {
                assert_eq!(chip_from_id(id), Some(chip), "{chip:?}");
            }
            // Sorted, non-empty and disjoint.
            let regions = chip.regions();
            for r in regions {
                assert!(r.start < r.end, "{chip:?} {}", r.name);
            }
            for w in regions.windows(2) {
                assert!(w[0].end <= w[1].start, "{chip:?} {}", w[1].name);
            }
        }
        // The ROM's data in the top of SRAM is mapped.
        assert_eq!(region_of(Chip::Esp32C3, 0x3FCD_F800).unwrap().kind, K::Dram);
        assert_eq!(region_of(Chip::Esp32C6, 0x4087_FF00).unwrap().kind, K::Sram);
        for chip in [Chip::Esp32S3, Chip::Esp32C2, Chip::Esp32C3] {
            assert_eq!(region_of(chip, 0x3FF0_0100).unwrap().kind, K::RomData);
        }
        // ROM1 on the data bus (ESP-IDF's SOC_MEM_INTERNAL_LOW on the ESP32;
        // the ROM ELF files load .rodata there).
        assert_eq!(
            region_of(Chip::Esp32, 0x3FF9_6000).unwrap().kind,
            K::RomData
        );
        assert_eq!(
            region_of(Chip::Esp32S2, 0x3FFA_C600).unwrap().kind,
            K::RomData
        );
    }

    #[test]
    fn rom_symbols_are_in_the_memory_map() {
        // The ROM's functions and data: in the ROM, its data bus alias, or
        // the RAM the ROM keeps for itself.
        let mut missing: Vec<_> = Chip::ALL
            .into_iter()
            .flat_map(|chip| {
                rom_symbol_set(chip)
                    .into_iter()
                    .filter(move |&(_, addr)| region_of(chip, addr).is_none())
                    .map(move |(name, addr)| (chip, addr, name))
            })
            .collect();
        missing.sort_by_key(|&(chip, addr, name)| (chip as u8, addr, name));
        assert!(missing.is_empty(), "{missing:#x?}");
    }

    #[test]
    fn guess_riscv_chip_from_addresses() {
        let guess = |addrs: &[u32]| guess_chip(Family::RiscV, addrs, None);
        // An ESP-IDF app: DROM, DRAM, IRAM and IROM.
        let c3 = [0x3C00_0020, 0x3FC8_8000, 0x4038_0000, 0x4200_0020];
        assert_eq!(guess(&c3), Chip::Esp32C3);
        // The C2's memory map is inside the C3's; only its ROM addresses
        // differ, and anything the C3 alone has wins.
        assert_eq!(guess(&[0x4038_0000, 0x4007_0000]), Chip::Esp32C2);
        assert_eq!(guess(&[0x403C_8000, 0x4007_0000]), Chip::Esp32C3);
        // Bootloaders: the C3's is above the C2's IRAM; the C2's is ambiguous.
        assert_eq!(guess(&[0x403C_E710, 0x3FCD_5710]), Chip::Esp32C3);
        assert_eq!(guess(&[0x403A_EB70, 0x3FCD_5B70]), Chip::Esp32C3);
        // Unified SRAM chips.
        assert_eq!(guess(&[0x4200_0020, 0x4080_0000]), Chip::Esp32C6);
        assert_eq!(guess(&[0x4086_E610]), Chip::Esp32C6);
        assert_eq!(guess(&[0x4083_EFD0]), Chip::Esp32C6); // an H2 bootloader
        assert_eq!(guess(&[0x4300_0020, 0x4080_0000]), Chip::Esp32C5);
        assert_eq!(guess(&[0x4200_0020]), Chip::Esp32C3);
    }

    /// `n` ROM symbols of `chip` that no chip in `others` has (same name at
    /// the same address).
    fn rom_fingerprint(chip: Chip, others: &[Chip], n: usize) -> Vec<(String, u32)> {
        let (base, _) = chip.rom_symbols();
        let mut out = Vec::new();
        for &(name, addr) in base {
            let elsewhere = others
                .iter()
                .any(|&o| rom_matches(o, &HashSet::from([(name, addr)])) > 0);
            if !elsewhere {
                out.push((name.to_string(), addr));
            }
            if out.len() == n {
                break;
            }
        }
        assert_eq!(out.len(), n, "{chip:?} has too few distinct ROM symbols");
        out
    }

    #[test]
    fn rom_fingerprints_pick_the_chip() {
        let guess = |family, segments: &[(u32, u32)], symbols: &[(String, u32)]| {
            guess_elf_chip(family, segments, None, symbols)
        };
        // Unified SRAM: the C6, H2 and C5 share these addresses.
        let sram = [(0x4080_0000, 0x1000), (0x4200_0020, 0x100)];
        let unified = [Chip::Esp32C6, Chip::Esp32H2, Chip::Esp32C5];
        assert_eq!(guess(Family::RiscV, &sram, &[]), (Chip::Esp32C6, 0));
        for chip in unified {
            let others: Vec<Chip> = unified.into_iter().filter(|&c| c != chip).collect();
            let symbols = rom_fingerprint(chip, &others, 3);
            assert_eq!(guess(Family::RiscV, &sram, &symbols), (chip, 3), "{chip:?}");
        }
        // The C2's memory map is inside the C3's.
        let iram = [(0x4038_0000, 0x1000), (0x3FCA_0000, 0x100)];
        let c2 = rom_fingerprint(Chip::Esp32C2, &[Chip::Esp32C3], 4);
        assert_eq!(guess(Family::RiscV, &iram, &c2), (Chip::Esp32C2, 4));
        assert_eq!(guess(Family::RiscV, &iram, &[]), (Chip::Esp32C3, 0));
        // A ROM symbol that several of a chip's tables list counts once:
        // the C3's ECO3 and ECO7 tables both have wDevCheckBlockError, but
        // that one C3 symbol does not tie two of the C2's.
        let shared = ("wDevCheckBlockError", 0x4000_17B4);
        let (_, c3_revisions) = Chip::Esp32C3.rom_symbols();
        assert!(
            c3_revisions
                .iter()
                .filter(|(_, t)| t.contains(&shared))
                .count()
                > 1
        );
        let mixed = [
            ("GetSecurityInfoProc".to_string(), 0x4000_00B0),
            ("MMU_Get_Page_Mode".to_string(), 0x4000_0700),
            (shared.0.to_string(), shared.1),
        ];
        assert_eq!(guess(Family::RiscV, &iram, &mixed), (Chip::Esp32C2, 2));
        // Symbols that are no chip's ROM symbols change nothing.
        let junk = [
            ("memcpy".to_string(), 0x1234),
            ("x".to_string(), 0x4000_0000),
        ];
        assert_eq!(guess(Family::RiscV, &sram, &junk), (Chip::Esp32C6, 0));
        // A chip whose memory map does not hold the program is never picked,
        // however its ROM symbols match.
        let c5 = rom_fingerprint(Chip::Esp32C5, &[Chip::Esp32C6], 3);
        let c6_only = [(0x4087_0000, 0x100)];
        assert_eq!(guess(Family::RiscV, &c6_only, &c5).0, Chip::Esp32C6);
        // Xtensa: the memory map already decides; ROM symbols agree.
        let esp32 = [(0x4008_0000, 0x100), (0x3FFB_0000, 0x100)];
        let symbols = rom_fingerprint(Chip::Esp32, &[], 2);
        assert_eq!(guess(Family::Xtensa, &esp32, &symbols), (Chip::Esp32, 2));
        assert_eq!(guess(Family::Xtensa, &esp32, &[]), (Chip::Esp32, 0));
    }

    #[test]
    fn fallback_is_a_chip_that_fits() {
        let guess = |segments: &[(u32, u32)]| guess_elf_chip(Family::RiscV, segments, None, &[]);
        // The flash segment starts in every unified-SRAM chip's window but
        // only the C5's (32 MB) holds its end; the address heuristic, which
        // looks at where segments start, says C6.
        let segments = [(0x4080_0000, 0x100), (0x42FF_FF00, 0x200)];
        assert_eq!(guess_riscv_chip(&[0x4080_0000, 0x42FF_FF00]), Chip::Esp32C6);
        assert_eq!(guess(&segments), (Chip::Esp32C5, 0));
        // The same start with an end inside every window keeps the heuristic.
        assert_eq!(
            guess(&[(0x4080_0000, 0x100), (0x42FF_FF00, 0x80)]),
            (Chip::Esp32C6, 0)
        );
        // SRAM running past the H2's end: the heuristic's C6 still fits.
        assert_eq!(guess(&[(0x4084_FF00, 0x200)]), (Chip::Esp32C6, 0));
        // Fitting no chip, every chip is a candidate and the heuristic decides.
        assert_eq!(guess(&[(0x4087_FF00, 0x200)]), (Chip::Esp32C6, 0));
    }

    #[test]
    fn segments_fit_memory_maps() {
        assert!(fits_memory_map(Chip::Esp32C3, &[(0x4200_0020, 0x1000)]));
        // Running off the end of the C6's SRAM, or into unmapped memory.
        assert!(!fits_memory_map(Chip::Esp32C6, &[(0x4087_F000, 0x2000)]));
        assert!(!fits_memory_map(Chip::Esp32C3, &[(0x1_0000, 0x100)]));
        // Wrapping around the address space.
        assert!(!fits_memory_map(
            Chip::Esp32C3,
            &[(0x6000_0000, 0xFFFF_FFFF)]
        ));
        // Empty segments only need their address mapped.
        assert!(fits_memory_map(Chip::Esp32C3, &[(0x3FC8_8000, 0)]));
        // Both ends mapped, but with unmapped memory between them: DROM to DRAM.
        assert!(!fits_memory_map(
            Chip::Esp32C3,
            &[(0x3C00_0020, 0x3FC8_8000 - 0x3C00_0020)]
        ));
        // Spanning adjacent regions is fine: the ESP32's DRAM runs into its ROM.
        assert!(fits_memory_map(Chip::Esp32, &[(0x3FFF_FFF0, 0x20)]));
        // ... and up to the very last byte of a region, but not past it.
        assert!(fits_memory_map(Chip::Esp32C3, &[(0x3C00_0000, 0x80_0000)]));
        assert!(!fits_memory_map(Chip::Esp32C3, &[(0x3C00_0000, 0x80_0001)]));
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
        // Written at a known flash offset (a partition of a dump): in the
        // flash window that offset is in.
        let mut at = i.clone();
        at.set_flash_offset(0x11_0000);
        assert_eq!(at.segments[0].addr, 0x4021_0010);
        // Other images do not depend on it.
        let mut v1 = parse_image(&d[16 + 8..], 0).unwrap();
        let before = v1.clone();
        v1.set_flash_offset(0x10000);
        assert_eq!(v1, before);
    }

    #[test]
    fn guess_xtensa_chip_from_entry_hint() {
        let guess = |addrs: &[u32], hint| guess_chip(Family::Xtensa, addrs, hint);
        let esp8266 = [0x4010_0000, 0x3FFE_8000];
        assert_eq!(guess(&esp8266, Some(EntryHint::Call0)), Chip::Esp8266);
        assert_eq!(guess(&esp8266, Some(EntryHint::Windowed)), Chip::Esp32);
        assert_eq!(guess(&esp8266, None), Chip::Esp8266);
        // call0 code outside the ESP8266's memory map: an ESP32 startup stub.
        assert_eq!(guess(&[0x4008_0000], Some(EntryHint::Call0)), Chip::Esp32);
        // Addresses only one chip has win over the hint.
        assert_eq!(guess(&[0x4200_0000], Some(EntryHint::Call0)), Chip::Esp32S3);
        assert_eq!(guess(&[0x3F00_0000], None), Chip::Esp32S2);
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
    fn unsupported_chip_in_selected_partition() {
        // A supported factory app, an ESP32-C61 ota_0 app and an ESP32-P4
        // bootloader.
        let mut d = dump(&[
            ("factory", 0x00, 0x10000, 0x1000),
            ("ota_0", 0x10, 0x11000, 0x1000),
        ]);
        let c61 = image(0xE9, Some(ext(20)), 0x4080_0000, &[(0x4080_0000, &[0; 4])]);
        let p4 = image(0xE9, Some(ext(18)), 0x4FF0_0000, &[(0x4FF0_0000, &[0; 4])]);
        d[..p4.len()].copy_from_slice(&p4);
        d.extend(s3_app(0x4037_5000));
        d.resize(0x11000, 0xFF);
        d.extend(&c61);
        let found = |sel| find_unsupported(&d, sel);
        let at = |start, chip_id| Some(UnsupportedImage { start, chip_id });
        assert_eq!(found(Selection::Partition("ota_0")), at(0x11000, 20));
        assert_eq!(found(Selection::Partition("factory")), None);
        assert_eq!(found(Selection::Partition("nope")), None);
        assert_eq!(found(Selection::Bootloader), at(0, 18));
        // By default, the first app partition with such an image.
        assert_eq!(found(Selection::Default), at(0x11000, 20));
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

    #[test]
    fn esp8266_flash_window() {
        let app = image(
            0xE9,
            None,
            0x4010_0004,
            &[
                (0x4010_0000, &[0x12, 0xc1, 0xf0, 0]),
                (0x3FFE_8000, &[0; 16]),
            ],
        );
        let window = |d: &[u8]| {
            find_firmware(d, Selection::Default)
                .unwrap()
                .map_flash_window
        };
        // NONOS: irom0text.bin flashed at 0x10000 (or 0x40000).
        let mut nonos = app.clone();
        nonos.resize(0x20000, 0xFF);
        nonos[0x10000..0x10004].copy_from_slice(&[0x12, 0xc1, 0xf0, 0]);
        assert!(window(&nonos));
        let mut at_40000 = app.clone();
        at_40000.resize(0x50000, 0xFF);
        at_40000[0x40000] = 0;
        assert!(window(&at_40000));
        // An app read out of its partition, padded with erased flash.
        let mut padded = app.clone();
        padded.resize(0x30000, 0xFF);
        assert!(!window(&padded));
        assert!(!window(&app));
        // An image that maps its flash itself (RTOS SDK v3).
        let irom = image(
            0xE9,
            None,
            0x4010_0004,
            &[
                (0x4010_0000, &[0x12, 0xc1, 0xf0, 0]),
                (0x4021_0010, &[0; 16]),
            ],
        );
        let mut rtos = irom.clone();
        rtos.resize(0x20000, 0);
        assert!(!window(&rtos));
    }

    fn table_entry(kind: u8, subtype: u8, offset: u32, size: u32, label: &[u8]) -> Vec<u8> {
        let mut e = vec![0xAA, 0x50, kind, subtype];
        e.extend(offset.to_le_bytes());
        e.extend(size.to_le_bytes());
        let mut name = [0u8; 16];
        name[..label.len()].copy_from_slice(label);
        e.extend(name);
        e.extend([0; 4]);
        e
    }

    /// A partition table sector holding these entries, then `after`.
    fn table(entries: &[Vec<u8>], after: &[u8]) -> Vec<u8> {
        let mut t: Vec<u8> = entries.concat();
        t.extend(after);
        t.resize(0x1000, 0xFF);
        t
    }

    #[test]
    fn partition_tables() {
        let nvs = table_entry(1, 2, 0x9000, 0x6000, b"nvs");
        let factory = table_entry(0, 0, 0x10000, 0x100000, b"factory");
        let parts = parse_partition_table(&table(&[nvs.clone(), factory.clone()], &[])).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[1],
            Partition {
                kind: 0,
                subtype: 0,
                offset: 0x10000,
                size: 0x100000,
                label: "factory".into()
            }
        );
        // The MD5 entry gen_esp32part.py writes after the entries.
        let mut md5 = vec![0xEB, 0xEB];
        md5.extend([0xFF; 14]);
        md5.extend([0x5A; 16]);
        let t = table(&[nvs.clone(), factory.clone()], &md5);
        assert_eq!(parse_partition_table(&t).map(|p| p.len()), Some(2));
        // The table may be all the data there is.
        assert_eq!(parse_partition_table(&nvs).map(|p| p.len()), Some(1));
        // Custom types, and labels with spaces.
        let custom = table_entry(0x40, 0x01, 0x20000, 0x1000, b"my data");
        assert!(parse_partition_table(&table(&[custom], &[])).is_some());
    }

    #[test]
    fn not_partition_tables() {
        let ok = table_entry(1, 2, 0x9000, 0x6000, b"nvs");
        let bad = [
            table_entry(1, 2, 0x9800, 0x6000, b"nvs"), // not 4 KiB-aligned
            table_entry(1, 2, 0x8000, 0x1000, b"nvs"), // the table's own sector
            table_entry(1, 2, 0x1000, 0x1000, b"nvs"), // before the table
            table_entry(1, 2, 0x9000, 0, b"nvs"),      // empty
            table_entry(1, 2, 0xFFFF_F000, 0x2000, b"nvs"), // past 4 GiB
            table_entry(0x10, 0, 0x9000, 0x6000, b"nvs"), // reserved type
            table_entry(0xFF, 0, 0x9000, 0x6000, b"nvs"), // erased type
            table_entry(1, 2, 0x9000, 0x6000, b""),    // no label
            table_entry(1, 2, 0x9000, 0x6000, b"nv\x01s"), // unprintable label
        ];
        for entry in bad {
            assert_eq!(
                parse_partition_table(&table(&[ok.clone(), entry], &[])),
                None
            );
        }
        // Something other than an entry, the MD5 entry or erased flash after
        // the entries.
        assert_eq!(
            parse_partition_table(&table(std::slice::from_ref(&ok), &[0; 32])),
            None
        );
        // No entries at all.
        assert_eq!(parse_partition_table(&table(&[], &[])), None);
        // Only the magic, as in a file that happens to have it there.
        let mut d = vec![0u8; 0x9000];
        d[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 2].copy_from_slice(&[0xAA, 0x50]);
        assert_eq!(partition_table(&d), None);
    }

    #[test]
    fn partition_table_of_a_dump() {
        for base in DUMP_BASES {
            let mut d = vec![0xFF; 0x9000 - base];
            let t = table(&[table_entry(0, 0, 0x10000, 0x1000, b"factory")], &[]);
            d[PARTITION_TABLE_OFFSET - base..].copy_from_slice(&t);
            let (found, parts) = partition_table(&d).unwrap();
            assert_eq!((found, parts[0].label.as_str()), (base, "factory"));
        }
        // An app image is not a dump.
        assert_eq!(partition_table(&s3_app(0x4037_5000)), None);
    }
}
