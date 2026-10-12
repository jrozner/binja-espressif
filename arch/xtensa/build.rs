fn main() {
    let link_path = std::env::var_os("DEP_BINARYNINJACORE_PATH")
        .expect("DEP_BINARYNINJACORE_PATH not specified");

    println!("cargo::rustc-link-lib=dylib=binaryninjacore");
    println!("cargo::rustc-link-search={}", link_path.to_str().unwrap());

    // The rpath lets binaries linked against an installed core (tests) find
    // it. A stub-core build is loaded by a Binary Ninja whose core is
    // already in the process, so it gets no rpath into the build directory.
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
