use std::io::Write;
use std::path::{Path, PathBuf};

/// The chips whose Espressif SVD file (from the `svd` submodule) is embedded,
/// gzip-compressed, in the plugin; see src/svd.rs.
const SVD_CHIPS: [&str; 7] = [
    "esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c6", "esp32h2",
];

fn compress_svd_files() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    for chip in SVD_CHIPS {
        let source = manifest.join("svd").join("svd").join(format!("{chip}.svd"));
        println!("cargo::rerun-if-changed={}", source.display());
        let svd = std::fs::read(&source).unwrap_or_else(|err| {
            panic!(
                "cannot read {}: {err}\nThe Espressif SVD files are a git submodule; run `git submodule update --init`.",
                source.display()
            )
        });
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(&svd).unwrap();
        write_if_changed(
            &out.join(format!("{chip}.svd.gz")),
            &encoder.finish().unwrap(),
        );
    }
}

fn write_if_changed(path: &Path, data: &[u8]) {
    if std::fs::read(path).ok().as_deref() != Some(data) {
        std::fs::write(path, data).unwrap();
    }
}

fn main() {
    compress_svd_files();

    let link_path = std::env::var_os("DEP_BINARYNINJACORE_PATH")
        .expect("DEP_BINARYNINJACORE_PATH not specified");

    println!("cargo::rustc-link-lib=dylib=binaryninjacore");
    println!("cargo::rustc-link-search={}", link_path.to_str().unwrap());

    // The rpath lets binaries linked against an installed core (tests,
    // examples) find it. A stub-core build is a release plugin, loaded by a
    // Binary Ninja whose core is already in the process, so it gets no rpath
    // into the build directory.
    #[cfg(target_os = "linux")]
    if std::env::var_os("CARGO_FEATURE_CORE").is_some() {
        println!(
            "cargo::rustc-link-arg=-Wl,-rpath,{0},-L{0}",
            link_path.to_string_lossy()
        );
    }

    #[cfg(target_os = "macos")]
    {
        let crate_name = std::env::var("CARGO_PKG_NAME").expect("CARGO_PKG_NAME not set");
        let lib_name = crate_name.replace('-', "_");
        println!("cargo::rustc-link-arg=-Wl,-install_name,@rpath/lib{lib_name}.dylib");
    }
}
