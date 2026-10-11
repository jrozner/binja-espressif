//! Automatic loading of Espressif's SVD files.
//!
//! Binary Ninja's bundled SVD loader (`svd_ninja`) runs as an analysis step
//! (`analysis.svd.loader`) and applies the file named by the
//! `analysis.svd.autoLoadFile` setting of the view. This plugin registers a
//! step right before it that, for ESP views and ELF files on a chip
//! platform, points that setting at
//! Espressif's SVD for the chip, bundled with the plugin. The files are
//! vendored in `svd/` (from https://github.com/espressif/svd and, for the
//! ESP32-C5, https://github.com/espressif/esp-bist; Apache-2.0; see
//! svd/README.md), are gzip-compressed by build.rs and embedded, and are
//! written to a cache in the user directory on first use (see `cache`), because the
//! loader takes a path.
//!
//! An SVD the user chose is left alone, and `espressif.svd.autoLoad` turns the
//! behaviour off. This has to happen in an analysis step rather than when the
//! view is initialized: the options given to *Open with Options* are applied
//! to the view only after initialization, so the view cannot see them yet.
//! Without the SVD plugin the step is not registered and nothing happens.

use std::io::Read;
use std::path::PathBuf;

use binaryninja::binary_view::{BinaryView, CustomBinaryViewType};
use binaryninja::settings::{QueryOptions, Settings, SettingsScope};
use binaryninja::workflow::{Activity, AnalysisContext, Workflow, activity};

use crate::espressif::{Chip, cache};

/// The SVD loader's analysis step and setting.
const SVD_LOADER: &str = "analysis.svd.loader";
const SVD_FILE: &str = "analysis.svd.autoLoadFile";
/// This plugin's switch.
pub const AUTO_LOAD: &str = "espressif.svd.autoLoad";

// Compressed from svd/ by build.rs.
pub(super) static ESP32: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/esp32.svd.gz"));
pub(super) static ESP32S2: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/esp32s2.svd.gz"));
pub(super) static ESP32S3: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/esp32s3.svd.gz"));
pub(super) static ESP32C2: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/esp32c2.svd.gz"));
pub(super) static ESP32C3: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/esp32c3.svd.gz"));
pub(super) static ESP32C6: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/esp32c6.svd.gz"));
pub(super) static ESP32H2: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/esp32h2.svd.gz"));
pub(super) static ESP32C5: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/esp32c5.svd.gz"));

pub fn register_settings() {
    let settings = Settings::global();
    settings.register_group("espressif", "Espressif");
    settings.register_setting_json(
        AUTO_LOAD,
        r#"{
            "title": "Load Espressif SVD automatically",
            "type": "boolean",
            "default": true,
            "description": "Have the SVD loader apply Espressif's SVD file for the chip (bundled with this plugin) to ESP firmware images, naming and typing the peripheral registers. Ignored if an SVD file is chosen explicitly. Requires the SVD loader plugin.",
            "ignore": ["SettingsProjectScope"]
        }"#,
    );
}

fn cache_dir() -> PathBuf {
    cache::dir("svd")
}

/// Writes the chip's SVD file to the cache (once) and returns its path.
fn extract(chip: Chip) -> Option<PathBuf> {
    let compressed = chip.bundled_svd()?;
    let result = cache::extract("svd", chip.platform_name(), "svd", compressed, || {
        let mut svd = Vec::new();
        flate2::read::GzDecoder::new(compressed).read_to_end(&mut svd)?;
        Ok(svd)
    });
    match result {
        Ok(path) => Some(path),
        Err(err) => {
            tracing::warn!(
                "could not write the {} SVD file to {}: {err}",
                chip.display_name(),
                cache_dir().display()
            );
            None
        }
    }
}

/// Registers the analysis step that selects the SVD file, before the SVD
/// loader's own step. Needs the SVD loader to be registered already (see the
/// plugin dependency in lib.rs).
pub fn register_activity() {
    let config = activity::Config::action(
        "espressif.svd.select",
        "Select Espressif SVD",
        "Points the SVD loader at Espressif's SVD file for the chip of an ESP firmware image or ELF file.",
    )
    .eligibility(activity::Eligibility::auto().run_once(true));
    let select = Activity::new_with_action(config, |ctx: &AnalysisContext| {
        let view = ctx.view();
        let view_type = view.view_type();
        if view_type != crate::espressif::view::EspViewType::NAME && view_type != "ELF" {
            return;
        }
        let Some(platform) = view.default_platform() else {
            return;
        };
        let name = platform.name();
        if let Some(chip) = Chip::from_platform_name(&name) {
            configure(&view, chip);
        }
    });
    let registered = Workflow::cloned("core.module.metaAnalysis")
        .ok_or(())
        .and_then(|w| w.activity_before(&select, SVD_LOADER))
        .and_then(|w| w.register());
    if registered.is_err() {
        tracing::info!(
            "the SVD loader is not available; Espressif SVD files will not be loaded automatically"
        );
    }
}

/// Points the SVD loader at the chip's SVD file for `view`, unless it is
/// disabled, unavailable or the user chose a file.
fn configure(view: &BinaryView, chip: Chip) {
    let settings = Settings::global();
    if chip.bundled_svd().is_none() || !settings.contains(SVD_FILE) {
        return;
    }
    let mut query = QueryOptions::new_with_view(view);
    if settings.contains(AUTO_LOAD) && !settings.get_bool_with_opts(AUTO_LOAD, &mut query) {
        return;
    }
    // A file the user chose wins; one of ours (from an earlier open of this
    // database) is refreshed in case the cache was cleared.
    let current = settings.get_string_with_opts(SVD_FILE, &mut query);
    if !current.is_empty() && !PathBuf::from(&current).starts_with(cache_dir()) {
        return;
    }
    let Some(path) = extract(chip) else { return };
    let path = path.to_string_lossy();
    settings.set_string_with_opts(
        SVD_FILE,
        &path,
        &QueryOptions::new_with_view(view).with_scope(SettingsScope::SettingsResourceScope),
    );
    tracing::info!(
        "loading Espressif's {} SVD file ({path})",
        chip.display_name()
    );
}
