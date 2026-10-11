//! A cache, in the user directory, of the files the plugin embeds but Binary
//! Ninja only reads from a path: Espressif's SVD files and the ROM type
//! libraries.

use std::path::PathBuf;

/// The cache's directory for one kind of file.
pub fn dir(kind: &str) -> PathBuf {
    binaryninja::user_directory().join("espressif").join(kind)
}

/// FNV-1a, to version the cached files by content.
fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Writes the file that `contents` makes from the embedded data `embedded`
/// to the cache (once per version of the data) and returns its path:
/// `<kind>/<stem>-<hash of embedded>.<extension>`.
pub fn extract(
    kind: &str,
    stem: &str,
    extension: &str,
    embedded: &[u8],
    contents: impl FnOnce() -> std::io::Result<Vec<u8>>,
) -> std::io::Result<PathBuf> {
    let dir = dir(kind);
    let path = dir.join(format!("{stem}-{:016x}.{extension}", fnv1a(embedded)));
    if path.is_file() {
        return Ok(path);
    }
    let data = contents()?;
    std::fs::create_dir_all(&dir)?;
    // Write and rename, so a concurrent open never sees a partial file.
    let partial = dir.join(format!(".{stem}.{}", std::process::id()));
    std::fs::write(&partial, &data)?;
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_reference_values() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
