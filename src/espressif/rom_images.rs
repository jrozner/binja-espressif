//! Espressif's mask ROM images, from the ROM ELF files Espressif publishes
//! in esp-rom-elfs (https://github.com/espressif/esp-rom-elfs, Apache-2.0;
//! the ROM code itself is closed source and contains third-party
//! components, see that repository's README).
//!
//! The plugin does not bundle them. With an image of the chip's ROM at hand,
//! the ROM code region of [`rom_types`](super::rom_types) holds the real ROM
//! instead of zeros; without one, it stays zero-filled. This module finds the
//! files and reads them; `rom_download` downloads them, with the user's
//! consent, and wires this into Binary Ninja.
//!
//! The ROM ELF files are named `<chip>_rev<REV>_rom.elf`, where `REV` is the
//! chip revision (major * 100 + minor) whose ROM it is. A view wants the ROM
//! of the minimum chip revision its image requires, or the lowest revision
//! for an ELF file (which does not say), unless the `espressif.rom.revision`
//! setting says otherwise; the file used is the one of the highest revision
//! up to that, or else the lowest one there is (see [`pick_revision`]).
//!
//! Directories are searched in order (see [`search_dirs`]): the one the user
//! set, ESP-IDF's tools install (`$IDF_TOOLS_PATH`, `~/.espressif`), then the
//! plugin's own download cache. The first file found that passes
//! [`RomElf::parse`]'s checks against the chip is used.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::espressif::Chip;
use crate::espressif::elf::elf_symbols;

/// A release of esp-rom-elfs: the archive the plugin downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    /// The release tag.
    pub tag: &'static str,
    /// Where the archive is downloaded from (HTTPS).
    pub url: &'static str,
    /// The archive's SHA-256, as the release's checksum file gives it.
    pub sha256: &'static str,
    /// The archive's size in bytes.
    pub size: u64,
}

/// The release the plugin downloads; nothing downloaded is used unless its
/// SHA-256 is this one's.
pub const RELEASE: Release = Release {
    tag: "20260528",
    url: "https://github.com/espressif/esp-rom-elfs/releases/download/20260528/esp-rom-elfs-20260528.tar.gz",
    sha256: "caa463d3cbef2430a5a35847c1d9f2f152403b17a802050927ff60c8da54fe46",
    size: 4_902_355,
};

/// Where esp-rom-elfs comes from, the licence of its ELF files, and the
/// list of the third-party components in the ROMs (from esp-rom-elfs's
/// README), for the README and the consent prompt.
pub const SOURCE: &str = "https://github.com/espressif/esp-rom-elfs";
pub const LICENSE: &str = "Apache-2.0";
pub const NOTICE: &str = "https://docs.espressif.com/projects/esp-idf/en/latest/esp32/COPYRIGHT.html#rom-source-code-copyrights";

/// Limits on what is read from a (possibly malformed) archive or ELF file.
const MAX_ARCHIVE: usize = 64 << 20;
const MAX_MEMBER: usize = 16 << 20;
const MAX_SEGMENTS: usize = 256;

/// The name of the ROM ELF file of `chip` at revision `rev`.
pub fn member_name(chip: Chip, rev: u16) -> String {
    format!("{}_rev{rev}_rom.elf", chip.platform_name())
}

/// The chip and revision of a ROM ELF file named `<chip>_rev<REV>_rom.elf`,
/// for the chips the plugin supports. Nothing else matches: no directories,
/// no other chips, no other suffixes.
pub fn parse_member_name(name: &str) -> Option<(Chip, u16)> {
    let stem = name.strip_suffix("_rom.elf")?;
    let (chip, rev) = stem.rsplit_once("_rev")?;
    if rev.is_empty() || rev.len() > 5 || !rev.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // `rev0`, not `rev00`: one spelling per revision.
    if rev.len() > 1 && rev.starts_with('0') {
        return None;
    }
    Some((Chip::from_platform_name(chip)?, rev.parse().ok()?))
}

/// Of the revisions there are files for, the one to use for a view that
/// wants `wanted`: the highest one up to it, or else (nothing that old) the
/// lowest. `None` wants the lowest.
pub fn pick_revision(available: &[u16], wanted: Option<u16>) -> Option<u16> {
    let lowest = available.iter().copied().min();
    match wanted {
        None => lowest,
        Some(w) => available
            .iter()
            .copied()
            .filter(|&r| r <= w)
            .max()
            .or(lowest),
    }
}

/// The directories to look for ROM ELF files in, in order:
///
/// 1. `user`, the `espressif.rom.elfDirectory` setting, if set;
/// 2. ESP-IDF's tools install: `<IDF_TOOLS_PATH>/tools/esp-rom-elfs/<version>/`
///    and `~/.espressif/tools/esp-rom-elfs/<version>/`, newest version first
///    (the versions are dates);
/// 3. the plugin's download cache, `cache`.
///
/// `versions` lists the subdirectories of a directory.
pub fn search_dirs(
    user: &str,
    idf_tools_path: Option<PathBuf>,
    home: Option<PathBuf>,
    cache: PathBuf,
    versions: impl Fn(&Path) -> Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let user = user.trim();
    if !user.is_empty() {
        dirs.push(PathBuf::from(user));
    }
    let tools = [idf_tools_path, home.map(|h| h.join(".espressif"))];
    for root in tools.into_iter().flatten() {
        let root = root.join("tools").join("esp-rom-elfs");
        let mut found = versions(&root);
        found.sort();
        for dir in found.into_iter().rev() {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    if !dirs.contains(&cache) {
        dirs.push(cache);
    }
    dirs
}

/// The subdirectories of `dir`, or none if it cannot be read.
pub fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// The revisions of `chip` there are ROM ELF files for in `dir`.
fn revisions_in(dir: &Path, chip: Chip) -> Vec<u16> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file() || t.is_symlink()))
        .filter_map(|e| parse_member_name(&e.file_name().to_string_lossy()))
        .filter(|&(c, _)| c == chip)
        .map(|(_, rev)| rev)
        .collect()
}

/// A ROM image found on disk.
#[derive(Debug)]
pub struct Found {
    pub path: PathBuf,
    pub revision: u16,
    pub elf: RomElf,
}

/// The ROM image of `chip` for a view that wants revision `wanted` (see
/// [`pick_revision`]): the first one in `dirs` that is a valid ROM ELF file
/// of the chip. Files that are not are logged and skipped.
pub fn find(chip: Chip, wanted: Option<u16>, dirs: &[PathBuf]) -> Option<Found> {
    for dir in dirs {
        let mut available = revisions_in(dir, chip);
        while let Some(rev) = pick_revision(&available, wanted) {
            available.retain(|&r| r != rev);
            let path = dir.join(member_name(chip, rev));
            let data = match read_limited(&path) {
                Ok(data) => data,
                Err(err) => {
                    tracing::warn!("cannot read the ROM image {}: {err}", path.display());
                    continue;
                }
            };
            match RomElf::parse(&data, chip) {
                Ok(elf) => {
                    return Some(Found {
                        path,
                        revision: rev,
                        elf,
                    });
                }
                Err(err) => {
                    tracing::warn!(
                        "{} is not a {} ROM image: {err}",
                        path.display(),
                        chip.display_name()
                    );
                }
            }
        }
    }
    None
}

fn read_limited(path: &Path) -> std::io::Result<Vec<u8>> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_MEMBER as u64 {
        return Err(std::io::Error::other(format!("too large ({len} bytes)")));
    }
    std::fs::read(path)
}

/// The loadable contents of a chip's ROM ELF file: the bytes of its
/// `PT_LOAD` segments, at their load (physical) addresses, which are where
/// the ROM holds them. Code is loaded where it runs; the ROM's read-only
/// data and the initial values of its RAM data are loaded at their place in
/// the ROM, which is what the ROM's code region shows.
///
/// The ROM's read-only data is also kept at its virtual address, where the
/// ROM's code reads it: the ROM seen from the data bus, on the chips that
/// have such an alias (see [`RomElf::data_alias`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomElf {
    segments: Vec<(u32, Vec<u8>)>,
    /// The read-only segments loaded at another address than the ROM holds
    /// them at: (virtual address, bytes).
    aliased: Vec<(u32, Vec<u8>)>,
    /// The functions its symbol table names in the ROM's code, by address.
    functions: Vec<(String, u32)>,
    /// The other names it gives addresses in the ROM's code (labels, such
    /// as the jump table's `__call_*` entries).
    labels: Vec<(String, u32)>,
}

impl RomElf {
    /// Reads a ROM ELF file, checking that it is one of `chip`'s: a
    /// little-endian ELF32 executable for the chip's architecture, whose
    /// symbol table puts `ets_printf` and `memset` (or their ROM jump table
    /// entries, `__call_*`) where the chip's ROM table does, and whose
    /// contents include those addresses and lie in the ROM's code ranges.
    pub fn parse(file: &[u8], chip: Chip) -> Result<RomElf, String> {
        const ET_EXEC: u16 = 2;
        const PT_LOAD: u32 = 1;
        const PF_W: u32 = 2;
        let u16_at = |o: usize| file.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |o: usize| {
            file.get(o..o + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        };
        if file.get(0..4) != Some(b"\x7fELF") {
            return Err("not an ELF file".into());
        }
        if file.get(4..6) != Some(&[1, 1]) {
            return Err("not a little-endian ELF32 file".into());
        }
        let header = |o| u16_at(o).ok_or_else(|| "truncated ELF header".to_string());
        if header(0x10)? != ET_EXEC {
            return Err("not an executable".into());
        }
        let machine = header(0x12)? as u32;
        let family = chip.cpu().family();
        if machine != family.elf_machine() {
            return Err(format!(
                "its machine type is {machine}, not {} ({})",
                family.elf_machine(),
                family.name()
            ));
        }
        let phoff = u32_at(0x1c).ok_or("truncated ELF header")? as usize;
        let (phentsize, phnum) = (header(0x2a)? as usize, header(0x2c)? as usize);
        if phentsize < 32 || phnum > MAX_SEGMENTS {
            return Err("malformed program headers".into());
        }
        let mut segments = Vec::new();
        let mut aliased = Vec::new();
        for i in 0..phnum {
            let ph = phoff
                .checked_add(i * phentsize)
                .ok_or("malformed program headers")?;
            let field = |n: usize| u32_at(ph + 4 * n).ok_or("truncated program headers");
            if field(0)? != PT_LOAD {
                continue;
            }
            let (offset, vaddr, paddr) = (field(1)? as usize, field(2)?, field(3)?);
            let (filesz, flags) = (field(4)? as usize, field(6)?);
            if filesz == 0 {
                continue;
            }
            let bytes = offset
                .checked_add(filesz)
                .and_then(|end| file.get(offset..end))
                .ok_or("a segment lies outside the file")?;
            if paddr.checked_add(filesz as u32 - 1).is_none()
                || vaddr.checked_add(filesz as u32 - 1).is_none()
            {
                return Err("a segment wraps around the address space".into());
            }
            if vaddr != paddr && flags & PF_W == 0 {
                aliased.push((vaddr, bytes.to_vec()));
            }
            segments.push((paddr, bytes.to_vec()));
        }
        let ranges = chip.rom_code_ranges();
        let in_rom = |addr: u32| ranges.iter().any(|&(s, e)| s <= addr && addr < e);
        let symbols = elf_symbols(file);
        let (mut functions, mut labels): (Vec<_>, Vec<_>) = symbols
            .iter()
            .filter(|s| !s.absolute && !s.name.is_empty() && in_rom(s.value))
            .partition(|s| s.function);
        functions.sort_by_key(|s| s.value);
        labels.sort_by_key(|s| s.value);
        let pairs = |v: Vec<&crate::espressif::elf::ElfSymbol>| -> Vec<(String, u32)> {
            v.into_iter().map(|s| (s.name.clone(), s.value)).collect()
        };
        let elf = RomElf {
            segments,
            aliased,
            functions: pairs(functions),
            labels: pairs(labels),
        };

        if !elf
            .segments
            .iter()
            .any(|(addr, bytes)| in_rom(*addr) || in_rom(addr + bytes.len() as u32 - 1))
        {
            return Err("nothing in it lies in the ROM's code".into());
        }
        let (table, _) = chip.rom_symbols();
        for anchor in ["ets_printf", "memset"] {
            let Some(&(_, addr)) = table.iter().find(|&&(n, _)| n == anchor) else {
                continue;
            };
            let jump = format!("__call_{anchor}");
            let named = symbols
                .iter()
                .any(|s| s.value == addr && (s.name == anchor || s.name == jump));
            if !named {
                return Err(format!(
                    "it has no {anchor} at {addr:#x}, where the {} ROM has it",
                    chip.display_name()
                ));
            }
            if !elf.contains(addr) {
                return Err(format!("it has no contents at {anchor} ({addr:#x})"));
            }
        }
        Ok(elf)
    }

    /// The functions the image's symbol table names in the ROM's code, by
    /// address.
    pub fn functions(&self) -> &[(String, u32)] {
        &self.functions
    }

    /// The function a ROM function at `addr` is implemented by, with its
    /// name, if `addr` is an entry in the ROM's jump table, as in the
    /// RISC-V and ESP32-S3 ROMs: `__call_<name>`, which only jumps to
    /// `<name>` (`__call_memset` to `memset`, or `__call__rvfp__floatsidf`
    /// to `_rvfp__floatsidf` for the C6's `__floatsidf`).
    pub fn implementation(&self, addr: u32) -> Option<(&str, u32)> {
        let all = || self.functions.iter().chain(&self.labels);
        let target =
            all().find_map(|(n, a)| (*a == addr).then(|| n.strip_prefix("__call_")).flatten())?;
        all()
            .find(|(n, a)| n == target && *a != addr)
            .map(|(n, a)| (n.as_str(), *a))
    }

    /// Does the image have contents at `addr`?
    pub fn contains(&self, addr: u32) -> bool {
        self.segments
            .iter()
            .any(|(start, bytes)| *start <= addr && (addr - start) < bytes.len() as u32)
    }

    /// The read-only data the image loads in `start..end` at another
    /// address than the ROM holds it (the ROM's data bus alias, see
    /// [`RegionKind::RomData`]): the address of its first byte and the bytes
    /// up to its last, zero between segments; `None` if there is none.
    ///
    /// [`RegionKind::RomData`]: crate::espressif::image::RegionKind::RomData
    pub fn data_alias(&self, start: u32, end: u32) -> Option<(u32, Vec<u8>)> {
        let inside: Vec<_> = self
            .aliased
            .iter()
            .filter(|(addr, bytes)| {
                start <= *addr && *addr as u64 + bytes.len() as u64 <= end as u64
            })
            .collect();
        let first = inside.iter().map(|(addr, _)| *addr).min()?;
        let last = inside
            .iter()
            .map(|(addr, bytes)| *addr + bytes.len() as u32)
            .max()?;
        let mut out = vec![0u8; (last - first) as usize];
        for (addr, bytes) in inside {
            let at = (addr - first) as usize;
            out[at..at + bytes.len()].copy_from_slice(bytes);
        }
        Some((first, out))
    }

    /// The ROM's bytes at `start..start + len`, zero where the image has
    /// none.
    pub fn contents(&self, start: u32, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let end = start as u64 + len as u64;
        for (addr, bytes) in &self.segments {
            let (s, e) = (*addr as u64, *addr as u64 + bytes.len() as u64);
            let (from, to) = (s.max(start as u64), e.min(end));
            if from >= to {
                continue;
            }
            let src = &bytes[(from - s) as usize..(to - s) as usize];
            out[(from - start as u64) as usize..(to - start as u64) as usize].copy_from_slice(src);
        }
        out
    }
}

/// The lowercase hexadecimal SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Checks a downloaded archive against the release's size and SHA-256.
pub fn verify(data: &[u8], release: &Release) -> Result<(), String> {
    if data.len() as u64 != release.size {
        return Err(format!(
            "it is {} bytes, not the {} bytes of esp-rom-elfs {}",
            data.len(),
            release.size,
            release.tag
        ));
    }
    let sha256 = sha256_hex(data);
    if !sha256.eq_ignore_ascii_case(release.sha256) {
        return Err(format!(
            "its SHA-256 is {sha256}, not {} (esp-rom-elfs {})",
            release.sha256, release.tag
        ));
    }
    Ok(())
}

/// The regular files of a tar archive, by the name the archive gives them,
/// with their contents. Anything else (directories, links, devices, and the
/// metadata entries of the pax and GNU formats) is skipped, and a name that
/// a pax or GNU long-name entry would override is not trusted: the member
/// is skipped.
pub fn tar_files(tar: &[u8]) -> Result<Vec<(String, &[u8])>, String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut renamed = false;
    while at + 512 <= tar.len() {
        let header = &tar[at..at + 512];
        if header.iter().all(|&b| b == 0) {
            break; // end of archive
        }
        let checksum = octal(&header[148..156]).ok_or("malformed tar header checksum")?;
        let sum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, &b)| if (148..156).contains(&i) { b' ' } else { b } as u64)
            .sum();
        if sum != checksum {
            return Err(format!("bad tar header checksum at offset {at}"));
        }
        let size = octal(&header[124..136]).ok_or("malformed tar member size")? as usize;
        let data_start = at + 512;
        let data_end = data_start
            .checked_add(size)
            .filter(|&e| e <= tar.len())
            .ok_or("truncated tar member")?;
        let typeflag = header[156];
        let field = |b: &[u8]| {
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            String::from_utf8_lossy(&b[..end]).into_owned()
        };
        match typeflag {
            // pax extended headers and GNU long names override the next
            // member's name.
            b'x' | b'L' | b'K' => renamed = true,
            b'0' | 0 => {
                let mut name = field(&header[0..100]);
                if &header[257..262] == b"ustar" {
                    let prefix = field(&header[345..500]);
                    if !prefix.is_empty() {
                        name = format!("{prefix}/{name}");
                    }
                }
                if !renamed {
                    out.push((name, &tar[data_start..data_end]));
                }
                renamed = false;
            }
            _ => renamed = false,
        }
        at = data_start + size.div_ceil(512) * 512;
    }
    Ok(out)
}

fn octal(field: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(field).ok()?;
    let s = s.trim_matches(|c: char| c == '\0' || c == ' ');
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

/// The ROM ELF files to take from an archive's regular files: those named
/// `<chip>_rev<REV>_rom.elf` (at the top of the archive, or under `./`) for
/// a chip the plugin supports, and not too large. Nothing in a directory,
/// and no path that could leave the cache: the file is written under a
/// name made from the chip and revision alone.
pub fn select_members<'a>(files: &[(String, &'a [u8])]) -> Vec<(Chip, u16, &'a [u8])> {
    let mut out: Vec<(Chip, u16, &'a [u8])> = Vec::new();
    for (name, data) in files {
        let name = name.strip_prefix("./").unwrap_or(name);
        let Some((chip, rev)) = parse_member_name(name) else {
            continue;
        };
        if data.len() > MAX_MEMBER || out.iter().any(|&(c, r, _)| (c, r) == (chip, rev)) {
            continue;
        }
        out.push((chip, rev, data));
    }
    out
}

/// Verifies a downloaded esp-rom-elfs archive and unpacks its ROM ELF files
/// (see [`select_members`]) into `dest`. Returns how many it wrote.
pub fn unpack(archive: &[u8], release: &Release, dest: &Path) -> Result<usize, String> {
    use std::io::Read;
    verify(archive, release)?;
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(archive)
        .take(MAX_ARCHIVE as u64 + 1)
        .read_to_end(&mut tar)
        .map_err(|e| format!("cannot decompress it: {e}"))?;
    if tar.len() > MAX_ARCHIVE {
        return Err("it decompresses to too much data".into());
    }
    let files = tar_files(&tar)?;
    let members = select_members(&files);
    if members.is_empty() {
        return Err("it has no ROM ELF files".into());
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    for &(chip, rev, data) in &members {
        let path = dest.join(member_name(chip, rev));
        // Write and rename, so a concurrent open never sees a partial file.
        let partial = dest.join(format!(
            ".{}.{}",
            member_name(chip, rev),
            std::process::id()
        ));
        std::fs::write(&partial, data)
            .and_then(|_| std::fs::rename(&partial, &path))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(members.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_names() {
        assert_eq!(member_name(Chip::Esp32, 300), "esp32_rev300_rom.elf");
        assert_eq!(
            parse_member_name("esp32c3_rev101_rom.elf"),
            Some((Chip::Esp32C3, 101))
        );
        assert_eq!(
            parse_member_name("esp32_rev0_rom.elf"),
            Some((Chip::Esp32, 0))
        );
        for name in [
            "esp32c61_rev100_rom.elf", // not a supported chip
            "esp32p4_rev0_rom.elf",
            "esp32_rev_rom.elf",
            "esp32_rev00_rom.elf",
            "esp32_revx_rom.elf",
            "esp32_rev0_rom.elf.bak",
            "../esp32_rev0_rom.elf",
            "dir/esp32_rev0_rom.elf",
            "esp32_rev999999_rom.elf",
        ] {
            assert_eq!(parse_member_name(name), None, "{name}");
        }
    }

    #[test]
    fn revision_selection() {
        // ESP32: v0 and v3.0 ROMs.
        let esp32 = [0, 300];
        assert_eq!(pick_revision(&esp32, None), Some(0));
        assert_eq!(pick_revision(&esp32, Some(0)), Some(0));
        assert_eq!(pick_revision(&esp32, Some(100)), Some(0));
        assert_eq!(pick_revision(&esp32, Some(300)), Some(300));
        assert_eq!(pick_revision(&esp32, Some(301)), Some(300));
        // ESP32-C3: v0.0, v0.3 and v1.1, in any order.
        let c3 = [101, 0, 3];
        assert_eq!(pick_revision(&c3, Some(4)), Some(3));
        assert_eq!(pick_revision(&c3, Some(100)), Some(3));
        assert_eq!(pick_revision(&c3, Some(101)), Some(101));
        // ESP32-C2: nothing below v1.0, so the lowest there is.
        let c2 = [100, 200];
        assert_eq!(pick_revision(&c2, Some(0)), Some(100));
        assert_eq!(pick_revision(&c2, None), Some(100));
        assert_eq!(pick_revision(&[], Some(0)), None);
        assert_eq!(pick_revision(&[], None), None);
    }

    #[test]
    fn search_order() {
        let versions = |dir: &Path| -> Vec<PathBuf> {
            match dir.to_str().unwrap() {
                "/idf/tools/esp-rom-elfs" => vec!["/idf/tools/esp-rom-elfs/20230320".into()],
                "/home/me/.espressif/tools/esp-rom-elfs" => vec![
                    "/home/me/.espressif/tools/esp-rom-elfs/20230320".into(),
                    "/home/me/.espressif/tools/esp-rom-elfs/20240305".into(),
                ],
                _ => vec![],
            }
        };
        let dirs = search_dirs(
            " /my/roms ",
            Some("/idf".into()),
            Some("/home/me".into()),
            "/bn/espressif/rom-elfs/20260528".into(),
            versions,
        );
        let expected: Vec<PathBuf> = [
            "/my/roms",
            "/idf/tools/esp-rom-elfs/20230320",
            "/home/me/.espressif/tools/esp-rom-elfs/20240305",
            "/home/me/.espressif/tools/esp-rom-elfs/20230320",
            "/bn/espressif/rom-elfs/20260528",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(dirs, expected);
        // Nothing set and no ESP-IDF: only the cache.
        let dirs = search_dirs("", None, None, "/cache".into(), |_| vec![]);
        assert_eq!(dirs, [PathBuf::from("/cache")]);
        // IDF_TOOLS_PATH set to ~/.espressif: each directory once.
        let dirs = search_dirs(
            "",
            Some("/home/me/.espressif".into()),
            Some("/home/me".into()),
            "/cache".into(),
            versions,
        );
        assert_eq!(dirs.len(), 3);
    }

    /// A little-endian ELF32 executable for `machine` with the PT_LOAD
    /// segments `(paddr, bytes)` and a symbol table of `(name, value)`.
    pub(crate) fn rom_elf(
        machine: u16,
        segments: &[(u32, &[u8])],
        symbols: &[(&str, u32)],
    ) -> Vec<u8> {
        let mut f = vec![0u8; 0x34];
        f[..6].copy_from_slice(b"\x7fELF\x01\x01");
        f[0x10..0x12].copy_from_slice(&2u16.to_le_bytes());
        f[0x12..0x14].copy_from_slice(&machine.to_le_bytes());
        let phoff = f.len();
        f[0x1c..0x20].copy_from_slice(&(phoff as u32).to_le_bytes());
        f[0x2a..0x2c].copy_from_slice(&32u16.to_le_bytes());
        f[0x2c..0x2e].copy_from_slice(&(segments.len() as u16 + 1).to_le_bytes());
        // A RAM segment with no file contents, which is skipped.
        let mut phdrs = Vec::new();
        let mut data = Vec::new();
        let data_start = phoff + 32 * (segments.len() + 1);
        for (paddr, bytes) in segments {
            let mut ph = vec![0u8; 32];
            ph[0..4].copy_from_slice(&1u32.to_le_bytes());
            ph[4..8].copy_from_slice(&((data_start + data.len()) as u32).to_le_bytes());
            ph[8..12].copy_from_slice(&paddr.to_le_bytes());
            ph[12..16].copy_from_slice(&paddr.to_le_bytes());
            ph[16..20].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
            ph[20..24].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
            phdrs.extend(ph);
            data.extend_from_slice(bytes);
        }
        let mut bss = vec![0u8; 32];
        bss[0..4].copy_from_slice(&1u32.to_le_bytes());
        bss[8..12].copy_from_slice(&0x3ffe_0000u32.to_le_bytes());
        bss[20..24].copy_from_slice(&0x100u32.to_le_bytes());
        phdrs.extend(bss);
        f.extend(phdrs);
        f.extend(data);
        // Symbol table and its strings, and three section headers.
        let mut strtab = vec![0u8];
        let mut symtab = vec![0u8; 16];
        for &(name, value) in symbols {
            let mut sym = (strtab.len() as u32).to_le_bytes().to_vec();
            // Names ending in `()` are functions (STT_FUNC).
            let (name, kind) = match name.strip_suffix("()") {
                Some(name) => (name, 2),
                None => (name, 0),
            };
            strtab.extend(name.as_bytes());
            strtab.push(0);
            sym.extend(value.to_le_bytes());
            sym.extend([0, 0, 0, 0, kind, 0]);
            sym.extend(1u16.to_le_bytes());
            symtab.extend(sym);
        }
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

    const ESP32_PRINTF: u32 = 0x4000_7d54;
    const ESP32_MEMSET: u32 = 0x4000_c44c;

    fn esp32_rom() -> Vec<u8> {
        let code: Vec<u8> = (0..0x8000u32).map(|i| (i % 251) as u8 + 1).collect();
        rom_elf(
            94,
            &[(0x4000_7000, &code), (0x4006_6000, b"rodata!!")],
            &[("ets_printf", ESP32_PRINTF), ("memset", ESP32_MEMSET)],
        )
    }

    #[test]
    fn validates_the_chip() {
        let elf = esp32_rom();
        assert!(RomElf::parse(&elf, Chip::Esp32).is_ok());
        // Same machine, but the S2's ROM puts ets_printf elsewhere.
        let err = RomElf::parse(&elf, Chip::Esp32S2).unwrap_err();
        assert!(err.contains("ets_printf"), "{err}");
        // A RISC-V chip: the machine type is wrong.
        let err = RomElf::parse(&elf, Chip::Esp32C3).unwrap_err();
        assert!(err.contains("machine"), "{err}");
        // The symbols are right but the contents are not there.
        let missing = rom_elf(
            94,
            &[(0x4000_0000, &[1, 2, 3, 4])],
            &[("ets_printf", ESP32_PRINTF), ("memset", ESP32_MEMSET)],
        );
        let err = RomElf::parse(&missing, Chip::Esp32).unwrap_err();
        assert!(err.contains("no contents"), "{err}");
        // Not an ELF file; a truncated one.
        assert!(RomElf::parse(b"hello", Chip::Esp32).is_err());
        assert!(RomElf::parse(&elf[..0x40], Chip::Esp32).is_err());
        // An object file rather than an executable.
        let mut rel = elf.clone();
        rel[0x10] = 1;
        assert!(RomElf::parse(&rel, Chip::Esp32).is_err());
    }

    #[test]
    fn jump_table_entries_count_as_the_function() {
        // The RISC-V ROMs (and the S3's) export their API through a jump
        // table: the ROM table's ets_printf is the ELF's __call_ets_printf.
        let (table, _) = Chip::Esp32C3.rom_symbols();
        let addr = |n: &str| table.iter().find(|&&(name, _)| name == n).unwrap().1;
        let (printf, memset) = (addr("ets_printf"), addr("memset"));
        let code = vec![0x13u8; 0x1000];
        let elf = rom_elf(
            243,
            &[(0x4000_0000, &code)],
            &[("__call_ets_printf", printf), ("__call_memset", memset)],
        );
        assert!(RomElf::parse(&elf, Chip::Esp32C3).is_ok());
        // The C6 and H2 share ets_printf's address, not memset's.
        assert!(RomElf::parse(&elf, Chip::Esp32C6).is_err());
        assert!(RomElf::parse(&elf, Chip::Esp32H2).is_err());
    }

    #[test]
    fn jump_table_implementations() {
        let (table, _) = Chip::Esp32C6.rom_symbols();
        let addr = |n: &str| table.iter().find(|&&(name, _)| name == n).unwrap().1;
        let (printf, memset, floatsidf) = (addr("ets_printf"), addr("memset"), addr("__floatsidf"));
        let code = vec![0x13u8; 0x40000];
        let elf = rom_elf(
            243,
            &[(0x4000_0000, &code)],
            &[
                ("__call_ets_printf", printf),
                ("ets_printf()", 0x4001_9358),
                ("__call_memset", memset),
                ("memset()", 0x4003_0d9a),
                // The C6's soft-float helpers jump to `_rvfp_` labels.
                ("__call__rvfp__floatsidf", floatsidf),
                ("_rvfp__floatsidf", 0x4003_0344),
                ("helper()", 0x4000_1000),
                ("label", 0x4000_2000),
            ],
        );
        let rom = RomElf::parse(&elf, Chip::Esp32C6).unwrap();
        assert_eq!(rom.implementation(memset), Some(("memset", 0x4003_0d9a)));
        assert_eq!(
            rom.implementation(printf),
            Some(("ets_printf", 0x4001_9358))
        );
        assert_eq!(
            rom.implementation(floatsidf),
            Some(("_rvfp__floatsidf", 0x4003_0344))
        );
        // Not a jump table entry.
        assert_eq!(rom.implementation(0x4000_1000), None);
        // The functions to name: STT_FUNC symbols only, by address.
        let names: Vec<&str> = rom.functions().iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["helper", "ets_printf", "memset"]);
    }

    #[test]
    fn range_extraction() {
        let rom = RomElf::parse(&esp32_rom(), Chip::Esp32).unwrap();
        let (start, end) = Chip::Esp32.rom_code_ranges()[0];
        let all = rom.contents(start, (end - start) as usize);
        assert_eq!(all.len(), 0x70000);
        // Zeros before the code, the code, zeros, the ROM's data, zeros.
        assert!(all[..0x7000].iter().all(|&b| b == 0));
        assert_eq!(all[0x7000], 1);
        assert_eq!(all[0x7000 + 251], 1);
        assert!(all[0xf000..0x66000].iter().all(|&b| b == 0));
        assert_eq!(&all[0x66000..0x66008], b"rodata!!");
        assert!(all[0x66008..].iter().all(|&b| b == 0));
        // A window that starts inside a segment and ends past it.
        assert_eq!(
            rom.contents(0x4000_effe, 4),
            [all[0xeffe], all[0xefff], 0, 0]
        );
        assert_eq!(rom.contents(0x3fff_fffe, 3), [0, 0, 0]);
        assert!(rom.contains(ESP32_MEMSET) && !rom.contains(0x4000_0000));
    }

    #[test]
    fn data_bus_alias() {
        // The ROM's data, loaded where the data bus sees it (vaddr) and held
        // in the ROM (paddr), as in the ESP32's ROM ELF files.
        let (lo, hi) = (0x3FF9_0000, 0x3FFA_0000);
        let mut elf = esp32_rom();
        let rodata_vaddr = 0x34 + 32 + 8; // the second program header's p_vaddr
        assert!(
            RomElf::parse(&elf, Chip::Esp32)
                .unwrap()
                .data_alias(lo, hi)
                .is_none()
        );
        elf[rodata_vaddr..rodata_vaddr + 4].copy_from_slice(&0x3FF9_6000u32.to_le_bytes());
        let rom = RomElf::parse(&elf, Chip::Esp32).unwrap();
        assert_eq!(
            rom.data_alias(lo, hi),
            Some((0x3FF9_6000, b"rodata!!".to_vec()))
        );
        assert_eq!(rom.contents(0x4006_6000, 8), b"rodata!!");
        assert!(rom.data_alias(0x3FFA_0000, 0x3FFB_0000).is_none());
        assert!(rom.data_alias(lo, 0x3FF9_6004).is_none());
        // Writable data (the initial values of RAM data) is not the ROM's.
        let flags = 0x34 + 32 + 24; // its p_flags
        elf[flags..flags + 4].copy_from_slice(&6u32.to_le_bytes());
        let rom = RomElf::parse(&elf, Chip::Esp32).unwrap();
        assert!(rom.data_alias(lo, hi).is_none());
    }

    fn tar_entry(name: &str, typeflag: u8, data: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[156] = typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut out = h;
        out.extend(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    fn tar(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut out: Vec<u8> = entries.concat();
        out.extend([0u8; 1024]);
        out
    }

    #[test]
    fn tar_member_filtering() {
        let archive = tar(&[
            tar_entry("esp32_rev0_rom.elf", b'0', b"esp32"),
            tar_entry("./esp32s3_rev0_rom.elf", b'0', b"s3"),
            tar_entry("../esp32c3_rev3_rom.elf", b'0', b"traversal"),
            tar_entry("/tmp/esp32c3_rev3_rom.elf", b'0', b"absolute"),
            tar_entry("sub/esp32c6_rev0_rom.elf", b'0', b"nested"),
            tar_entry("esp32h2_rev0_rom.elf", b'2', b""), // a symlink
            tar_entry("esp32c2_rev100_rom.elf", b'5', b""), // a directory
            tar_entry("esp32p4_rev0_rom.elf", b'0', b"p4"),
            tar_entry("README.md", b'0', b"readme"),
            // A pax header that renames the next member: not trusted.
            tar_entry("PaxHeader", b'x', b"30 path=../../esp32_rev300.elf\n"),
            tar_entry("esp32_rev300_rom.elf", b'0', b"renamed"),
            tar_entry("esp32c5_rev0_rom.elf", 0, b"old-style"),
        ]);
        let files = tar_files(&archive).unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(!names.contains(&"esp32h2_rev0_rom.elf"));
        assert!(!names.contains(&"esp32_rev300_rom.elf"));
        let picked: Vec<(Chip, u16, &[u8])> = select_members(&files);
        assert_eq!(
            picked,
            [
                (Chip::Esp32, 0, &b"esp32"[..]),
                (Chip::Esp32S3, 0, &b"s3"[..]),
                (Chip::Esp32C5, 0, &b"old-style"[..]),
            ]
        );
    }

    #[test]
    fn malformed_tar() {
        let mut archive = tar(&[tar_entry("esp32_rev0_rom.elf", b'0', b"esp32")]);
        archive[0] ^= 1; // the checksum no longer matches
        assert!(tar_files(&archive).is_err());
        let archive = tar(&[tar_entry("esp32_rev0_rom.elf", b'0', &[7; 600])]);
        assert!(tar_files(&archive[..512 + 100]).is_err()); // truncated
        assert!(tar_files(&[]).unwrap().is_empty());
    }

    #[test]
    fn checksum_verification() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let data = b"not the archive";
        let release = Release {
            tag: "test",
            url: "https://example.invalid/a.tar.gz",
            sha256: "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            size: 3,
        };
        assert!(verify(b"abc", &release).is_ok());
        assert!(verify(b"abd", &release).unwrap_err().contains("SHA-256"));
        assert!(verify(data, &release).unwrap_err().contains("bytes"));
        assert!(RELEASE.url.starts_with("https://"));
        assert_eq!(RELEASE.sha256.len(), 64);
    }

    #[test]
    fn unpacks_only_verified_archives() {
        let archive = tar(&[
            tar_entry("esp32_rev0_rom.elf", b'0', b"esp32"),
            tar_entry("../esp32_rev300_rom.elf", b'0', b"escape"),
        ]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &archive).unwrap();
        let gz = gz.finish().unwrap();
        let sha = sha256_hex(&gz);
        let sha: &'static str = Box::leak(sha.into_boxed_str());
        let release = Release {
            tag: "test",
            url: "https://example.invalid/a.tar.gz",
            sha256: sha,
            size: gz.len() as u64,
        };
        let dir =
            std::env::temp_dir().join(format!("binja-xtensa-rom-unpack-{}", std::process::id()));
        let dest = dir.join("cache");
        assert_eq!(unpack(&gz, &release, &dest), Ok(1));
        assert_eq!(
            std::fs::read(dest.join("esp32_rev0_rom.elf")).unwrap(),
            b"esp32"
        );
        assert!(!dir.join("esp32_rev300_rom.elf").exists());
        assert!(!dest.join("esp32_rev300_rom.elf").exists());
        // A corrupted archive is not unpacked.
        let mut bad = gz.clone();
        let n = bad.len();
        bad[n / 2] ^= 0xff;
        assert!(unpack(&bad, &release, &dir.join("bad")).is_err());
        assert!(!dir.join("bad").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Checks Espressif's real ROM ELF files, in the directory
    /// `$BN_XTENSA_ROM_ELFS` (an unpacked esp-rom-elfs release): each is
    /// valid for its own chip only, and has contents at the start of the
    /// ROM. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_rom_elfs() {
        use crate::espressif::image::RegionKind::RomData;
        let dir = PathBuf::from(std::env::var("BN_XTENSA_ROM_ELFS").expect("BN_XTENSA_ROM_ELFS"));
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let Some((chip, rev)) = parse_member_name(&name) else {
                continue;
            };
            let data = std::fs::read(&path).unwrap();
            for other in Chip::ALL
                .into_iter()
                .filter(|c| c.rom_type_library().is_some())
            {
                let parsed = RomElf::parse(&data, other);
                assert_eq!(
                    parsed.is_ok(),
                    other == chip,
                    "{name} as {other:?}: {parsed:?}"
                );
            }
            let elf = RomElf::parse(&data, chip).unwrap();
            let found = find(chip, Some(rev), std::slice::from_ref(&dir)).unwrap();
            assert_eq!(found.revision, rev, "{name}");
            for (start, end) in chip.rom_code_ranges() {
                let code = elf.contents(start, (end - start) as usize);
                assert!(
                    code[..16].iter().any(|&b| b != 0),
                    "{name} starts with zeros"
                );
            }
            // The ROM's data bus alias holds its .rodata, which is the
            // ROM's: the same bytes as the ROM holds.
            for region in chip.regions().iter().filter(|r| r.kind == RomData) {
                let (addr, data) = elf.data_alias(region.start, region.end).unwrap();
                let delta = elf
                    .aliased
                    .iter()
                    .find(|(a, _)| *a == addr)
                    .and_then(|(_, bytes)| {
                        elf.segments
                            .iter()
                            .find(|(_, b)| b == bytes)
                            .map(|(paddr, _)| paddr - addr)
                    })
                    .unwrap();
                assert_eq!(
                    elf.contents(addr + delta, data.len()),
                    data,
                    "{name}'s data bus alias"
                );
                assert!(data.len() > 0x1000, "{name}: {:#x} bytes", data.len());
            }
            checked += 1;
        }
        assert!(checked >= 8, "only {checked} ROM ELF files");
    }

    #[test]
    fn finds_the_first_valid_image() {
        let dir =
            std::env::temp_dir().join(format!("binja-xtensa-rom-find-{}", std::process::id()));
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        // `a` has a broken v0 ROM, `b` a good v0 and v3.0 ROM.
        std::fs::write(a.join("esp32_rev0_rom.elf"), b"broken").unwrap();
        std::fs::write(b.join("esp32_rev0_rom.elf"), esp32_rom()).unwrap();
        std::fs::write(b.join("esp32_rev300_rom.elf"), esp32_rom()).unwrap();
        std::fs::write(b.join("esp32s2_rev0_rom.elf"), esp32_rom()).unwrap();
        let dirs = [a.clone(), b.clone()];
        let found = find(Chip::Esp32, Some(0), &dirs).unwrap();
        assert_eq!(
            (found.path, found.revision),
            (b.join("esp32_rev0_rom.elf"), 0)
        );
        let found = find(Chip::Esp32, Some(301), &dirs).unwrap();
        assert_eq!(found.revision, 300);
        // The file named for the S2 holds the ESP32's ROM.
        assert!(find(Chip::Esp32S2, None, &dirs).is_none());
        assert!(find(Chip::Esp32S3, None, &dirs).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
