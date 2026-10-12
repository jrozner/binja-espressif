//! Using Espressif's ROM images in views, and downloading them, with the
//! user's consent.
//!
//! A view's ROM code regions (see [`rom_types`]) hold the chip's ROM image
//! if one is found (see [`rom_images`]) when they are added, and zeros
//! otherwise. An analysis step, [`register_activity`], maps the image into
//! zero-filled regions once one is available, so reanalyzing a view, or
//! reopening its file, picks up an image that was downloaded or configured
//! later; the same step offers the download when there is none. A database
//! keeps the region it was saved with; *Plugins > Espressif > Download ROM
//! Images* maps an image into one.
//!
//! Nothing is downloaded without consent:
//!
//! * In the UI, the first view that would use an image asks, once per
//!   session; a "No" is remembered for the release (in
//!   `<user dir>/espressif/rom-elfs.json`). After the download, the image is
//!   mapped into the view that asked; other open views map it when they are
//!   next reanalyzed or reopened. There is nothing to reload.
//! * The command *Plugins > Espressif > Download ROM Images* downloads on
//!   request (that request is the consent) and maps the image into its view.
//!   If a download is already running, it waits for that one instead.
//! * Headless, nothing is asked: the download happens only with the
//!   `espressif.rom.autoDownload` setting on (which also skips the question
//!   in the UI), and the hint about how to get an image is a warning, as
//!   nobody sees a prompt.
//!
//! In the UI, the download never blocks loading a view: it runs on a thread
//! of its own, and the view loads with zeros meanwhile. Headless, a load
//! waits for it (see [`download_inline`], at most [`INLINE_TIMEOUT`]), so
//! that the view, and a database saved right after the load, has the image:
//! a script or server that saves its analysis at once gets no second
//! chance. An automatic download that fails, or times out, leaves zeros and
//! is tried again by an analysis a few minutes later.
//!
//! The download goes through Binary Ninja's download provider (so its proxy
//! settings apply), over HTTPS, from the release pinned in [`RELEASE`], and
//! is used only if its size and SHA-256 match the pinned ones.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use binaryninja::background_task::BackgroundTask;
use binaryninja::binary_view::{BinaryView, CustomBinaryViewType, MetadataStoreFlags};
use binaryninja::command::{self, Command};
use binaryninja::download::{DownloadInstanceInputOutputCallbacks, DownloadProvider};
use binaryninja::interaction::{
    MessageBoxButtonResult, MessageBoxButtonSet, MessageBoxIcon, show_message_box,
};
use binaryninja::rc::Ref;
use binaryninja::settings::{QueryOptions, Settings};
use binaryninja::workflow::{Activity, AnalysisContext, Workflow, activity};

use crate::rom_images::{self, Found, LICENSE, NOTICE, RELEASE, Release, SOURCE};
use crate::{Chip, cache, rom_status, rom_types, view::EspViewType};

/// A directory of ROM ELF files, searched first.
pub const ELF_DIRECTORY: &str = "espressif.rom.elfDirectory";
/// The chip revision whose ROM to use, or empty for the view's own.
pub const REVISION: &str = "espressif.rom.revision";
/// Download without asking, and at all headless.
pub const AUTO_DOWNLOAD: &str = "espressif.rom.autoDownload";
/// Use ROM images at all.
pub const LOAD_IMAGES: &str = "espressif.rom.loadImages";
/// Create a function at each function a ROM image's symbol table names.
pub const CREATE_FUNCTIONS: &str = "espressif.rom.createFunctions";
/// The minimum chip revision of an ESP image, for the ROM revision.
const MIN_REVISION_METADATA: &str = "espressif.rom.minChipRevision";
/// Only for tests/test_headless.py: download from this URL, which must be
/// on a loopback address, instead of the release. The download is still
/// verified against [`RELEASE`].
const TEST_URL_ENV: &str = "BN_XTENSA_ROM_TEST_URL";
/// Only for tests/test_headless.py, with [`TEST_URL_ENV`]: wait this many
/// seconds for a headless download instead of [`INLINE_TIMEOUT`].
const TEST_TIMEOUT_ENV: &str = "BN_XTENSA_ROM_TEST_TIMEOUT";
/// How long a headless load waits for the automatic download (4.9 MB).
pub const INLINE_TIMEOUT: Duration = Duration::from_secs(30);

const TITLE: &str = "Espressif (binja-xtensa)";
const COMMAND: &str = "Espressif\\Download ROM Images";
/// The largest download accepted.
const MAX_DOWNLOAD: usize = 64 << 20;

/// What this session has done about missing ROM images, each at most once:
/// asked, explained how to get them. (Downloading them by itself is
/// [`AUTO_DOWNLOAD_STATE`].)
static ASKED: AtomicBool = AtomicBool::new(false);
static EXPLAINED: AtomicBool = AtomicBool::new(false);
/// The automatic download of this session.
static AUTO_DOWNLOAD_STATE: Mutex<AutoDownload> = Mutex::new(AutoDownload::NotTried);
/// Has an automatic download failed this session?
static AUTO_DOWNLOAD_FAILED: AtomicBool = AtomicBool::new(false);
/// How long after a failed automatic download another may start.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
/// Has this session said how to map an image into a database?
static DATABASE_NOTED: AtomicBool = AtomicBool::new(false);
/// The download that is running, and the last one's outcome (see
/// [`Downloads`]); signalled when one ends.
static DOWNLOADS: Mutex<Downloads> = Mutex::new(Downloads {
    running: None,
    last: None,
});
static DOWNLOAD_ENDED: Condvar = Condvar::new();

/// The download that is running, as its cancel flag, and the number of ROM
/// ELF files the last one unpacked, if it succeeded. A download that was
/// cancelled no longer counts as running: the provider has no deadline, so
/// it may never return, and it unpacks nothing if it does (see [`fetch`]).
struct Downloads {
    running: Option<Arc<AtomicBool>>,
    last: Option<usize>,
}

impl Downloads {
    fn busy(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|c| !c.load(Ordering::SeqCst))
    }
}
/// The automatic download a headless load started, for other loads to wait
/// for (see [`download_inline`]).
static INLINE: Mutex<Option<Arc<Inline>>> = Mutex::new(None);
/// Did this session's last download fail, or time out? (See
/// [`rom_status`].)
static DOWNLOAD_FAILED: AtomicBool = AtomicBool::new(false);

/// Where the automatic download stands: it is tried once per session, and
/// again a while after it fails (the network may be back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoDownload {
    NotTried,
    /// Running, or done.
    Started,
    /// Failed at this instant.
    Failed(Instant),
}

impl AutoDownload {
    /// May an automatic download start `now`?
    pub fn may_start(self, now: Instant) -> bool {
        match self {
            AutoDownload::NotTried => true,
            AutoDownload::Started => false,
            AutoDownload::Failed(at) => now.saturating_duration_since(at) >= RETRY_AFTER,
        }
    }
}

/// What a headless load that wants the ROM image does about the automatic
/// download (see [`download_inline`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlinePlan {
    /// Start it, and wait for it.
    Start,
    /// Wait for the one that is running.
    Wait,
    /// Load with zeros: it is done (and has no image for this view), or
    /// failed or timed out a short while ago.
    Skip,
}

/// [`download_inline`]'s rules, from the session's automatic download
/// `state` and the headless one that is running, if any (whether it timed
/// out and was cancelled).
pub fn plan_inline(state: AutoDownload, running: Option<bool>, now: Instant) -> InlinePlan {
    match running {
        Some(false) => InlinePlan::Wait,
        // Not running, or timed out: that failed (see `download_inline`),
        // and is retried a while later, even if it is stuck.
        _ if state.may_start(now) => InlinePlan::Start,
        _ => InlinePlan::Skip,
    }
}

/// A headless automatic download: its outcome once it is done, whether a
/// load that waited for it gave up, and whether a warning said it failed.
#[derive(Default)]
struct Inline {
    done: Mutex<Option<bool>>,
    finished: Condvar,
    cancel: Arc<AtomicBool>,
    warned: AtomicBool,
}

/// Locks `mutex`, whether or not a thread panicked holding it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn register_settings() {
    let settings = Settings::global();
    settings.register_group("espressif", "Espressif");
    settings.register_setting_json(
        LOAD_IMAGES,
        r#"{
            "title": "Map Espressif's ROM images",
            "type": "boolean",
            "default": true,
            "description": "Fill the mask ROM's code region with the chip's ROM image, from Espressif's ROM ELF files (esp-rom-elfs), when one is available: in the directory below, in an ESP-IDF tools install, or downloaded by this plugin. Without one, or with this off, the region holds zeros and only the ROM's typed functions.",
            "ignore": ["SettingsProjectScope", "SettingsResourceScope"]
        }"#,
    );
    settings.register_setting_json(
        CREATE_FUNCTIONS,
        r#"{
            "title": "Create Espressif ROM image functions",
            "type": "boolean",
            "default": true,
            "description": "When a ROM image is mapped, create a function, and analyze it, at each function its ROM ELF file's symbol table names. Off, those are only named, and analysis creates the ROM functions that code calls: a smaller database and a faster analysis.",
            "ignore": ["SettingsProjectScope", "SettingsResourceScope"]
        }"#,
    );
    settings.register_setting_json(
        ELF_DIRECTORY,
        r#"{
            "title": "Espressif ROM ELF directory",
            "type": "string",
            "default": "",
            "description": "A directory of Espressif's ROM ELF files (<chip>_rev<revision>_rom.elf, as in esp-rom-elfs), searched before ESP-IDF's tools install ($IDF_TOOLS_PATH or ~/.espressif/tools/esp-rom-elfs) and the plugin's download cache. Can be set per file with Open with Options (or as a load option headless), which a database saves.",
            "ignore": ["SettingsProjectScope"]
        }"#,
    );
    settings.register_setting_json(
        REVISION,
        r#"{
            "title": "Espressif ROM revision",
            "type": "string",
            "default": "",
            "description": "The chip revision whose ROM image to map, as v3.0 (or 3.0, or 300: major * 100 + minor); empty for the minimum revision an ESP image requires, or the lowest revision for an ELF file. The image of the highest revision up to it is used. Can be set per file with Open with Options.",
            "ignore": ["SettingsProjectScope"]
        }"#,
    );
    settings.register_setting_json(
        AUTO_DOWNLOAD,
        r#"{
            "title": "Download Espressif's ROM images automatically",
            "type": "boolean",
            "default": false,
            "description": "Let binja-xtensa download Espressif's ROM ELF files (esp-rom-elfs, Apache-2.0; the ROM code is closed source, with third-party components) from GitHub, without asking, when a view would use one and none is available (the download is verified against a SHA-256 hash shipped with the plugin). This is the only way it downloads in headless Binary Ninja.",
            "ignore": ["SettingsProjectScope", "SettingsResourceScope"]
        }"#,
    );
}

/// The chip of an ESP view or of an ELF file on a chip platform, if it has
/// a ROM image to map.
pub fn view_chip(view: &BinaryView) -> Option<Chip> {
    let view_type = view.view_type();
    if view_type != EspViewType::NAME && view_type != "ELF" {
        return None;
    }
    let chip = Chip::from_platform_name(&view.default_platform()?.name())?;
    chip.rom_type_library().is_some().then_some(chip)
}

/// The plugin's download cache for the pinned release.
pub fn cache_dir() -> PathBuf {
    cache::dir("rom-elfs").join(RELEASE.tag)
}

/// The view's [`ELF_DIRECTORY`]s, to search in this order: its own (a load
/// option, or the one its database was saved with), then the user's.
///
/// A view gets its own copy of each setting of the resource scope when it
/// is loaded (the load option's value, else the user's at that time), which
/// its database saves and which then hides the user's; the user's is
/// searched too, so that setting it once a file is open, or after a
/// database was saved, finds an image for it.
pub fn elf_directories(view: &BinaryView) -> [String; 2] {
    let settings = Settings::global();
    [
        settings.get_string_with_opts(ELF_DIRECTORY, &mut QueryOptions::new_with_view(view)),
        settings.get_string(ELF_DIRECTORY),
    ]
}

fn search_dirs(view: &BinaryView) -> Vec<PathBuf> {
    let user = elf_directories(view);
    let user = user.each_ref().map(String::as_str);
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let idf = std::env::var_os("IDF_TOOLS_PATH")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    rom_images::search_dirs(&user, idf, home, cache_dir(), rom_images::subdirectories)
}

/// Whether a download of the ROM images is running, and whether this
/// session's last one failed.
pub fn download_state() -> (bool, bool) {
    let downloading = lock(&DOWNLOADS).busy();
    (downloading, DOWNLOAD_FAILED.load(Ordering::SeqCst))
}

/// Records the minimum chip revision an ESP image requires, which picks
/// the ROM image's revision.
pub fn record_min_revision(view: &BinaryView, rev: u16) {
    view.store_metadata(
        MIN_REVISION_METADATA,
        rev as u64,
        MetadataStoreFlags::PERSISTENT,
    );
}

/// A chip revision as `v3.0`, `3.0` or `300` (major * 100 + minor).
pub fn parse_revision(text: &str) -> Option<u16> {
    let text = text.trim();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    let digits = |s: &str| !s.is_empty() && s.len() <= 4 && s.bytes().all(|b| b.is_ascii_digit());
    match text.split_once('.') {
        Some((major, minor)) if digits(major) && digits(minor) => {
            let (major, minor): (u16, u16) = (major.parse().ok()?, minor.parse().ok()?);
            (minor < 100).then_some(major.checked_mul(100)?.checked_add(minor)?)
        }
        None if digits(text) => text.parse().ok(),
        _ => None,
    }
}

/// The revision whose ROM the view wants: the setting's, the ESP image's
/// minimum, or (`None`) the lowest.
fn wanted_revision(view: &BinaryView) -> Option<u16> {
    let settings = Settings::global();
    let set = settings.get_string_with_opts(REVISION, &mut QueryOptions::new_with_view(view));
    if !set.trim().is_empty() {
        match parse_revision(&set) {
            Some(rev) => return Some(rev),
            None => tracing::warn!("'{REVISION}' is not a chip revision: {set:?}"),
        }
    }
    view.get_metadata::<u64>(MIN_REVISION_METADATA)
        .and_then(|r| u16::try_from(r).ok())
}

/// The chip's ROM image for the view, if ROM images are on and one is
/// available.
pub fn rom_image(view: &BinaryView, chip: Chip) -> Option<Found> {
    if !load_images(view) {
        return None;
    }
    rom_images::find(chip, wanted_revision(view), &search_dirs(view))
}

fn load_images(view: &BinaryView) -> bool {
    Settings::global().get_bool_with_opts(LOAD_IMAGES, &mut QueryOptions::new_with_view(view))
}

/// The chip's ROM image for a view that is being loaded or analyzed: as
/// [`rom_image`], but headless, with [`AUTO_DOWNLOAD`] on, one is
/// downloaded first if none is available, and the load waits for it (see
/// [`download_inline`]). In the UI, the download is offered later, on a
/// thread of its own (see [`offer`]).
///
/// This runs where the view's ROM region is added or refreshed (the view's
/// initialization, the ELF fix-ups, the image step), never in the analysis
/// of a function.
pub fn rom_image_for_load(view: &BinaryView, chip: Chip) -> Option<Found> {
    if let Some(found) = rom_image(view, chip) {
        return Some(found);
    }
    let plan = plan_offer(binaryninja::is_ui_enabled(), auto_download(), false);
    if plan != Offer::DownloadInline || !load_images(view) {
        return None;
    }
    // Seen only where the view has a region of zeros already (reanalysis):
    // a view being loaded gets its status once its region is added.
    rom_status::record_status(view, chip, rom_status::Status::Downloading);
    let downloaded = download_inline(chip);
    rom_status::record_unmapped(view, chip);
    if !downloaded {
        return None;
    }
    let found = rom_image(view, chip);
    if found.is_none() {
        tracing::warn!(
            "the ROM image download has no {} ROM image for this view",
            chip.display_name()
        );
    }
    found
}

/// Whether a ROM image's functions all become functions of the view (see
/// [`CREATE_FUNCTIONS`]).
pub fn create_functions(view: &BinaryView) -> bool {
    Settings::global().get_bool_with_opts(CREATE_FUNCTIONS, &mut QueryOptions::new_with_view(view))
}

/// Maps the chip's ROM image into the view's zero-filled ROM code regions
/// if one is available now; offers to download one if not.
///
/// A database keeps the ROM region it was saved with: it is reopened before
/// it is restored, so this cannot change it then. The command maps an image
/// into it once it is open (see [`map_into`]).
///
/// It also brings a database up to date with the ROM type library first
/// (see [`rom_types::update_database`]).
fn refresh(view: &BinaryView) {
    let Some(chip) = view_chip(view) else {
        return;
    };
    rom_types::backfill_rom_elf_imports(view, chip);
    rom_types::update_database(view, chip);
    if !rom_types::has_zero_filled_region(view, chip) {
        rom_status::backfill(view, chip);
        rom_status::sync_data_alias(view);
        return;
    }
    rom_status::record_unmapped(view, chip);
    if !load_images(view) {
        return;
    }
    if view.file().is_database_backed() {
        if !DATABASE_NOTED.swap(true, Ordering::SeqCst) {
            database_note(chip, binaryninja::is_ui_enabled());
        }
        return;
    }
    match rom_image_for_load(view, chip) {
        Some(found) => {
            rom_types::map_rom_image(view, chip, &found);
        }
        None => offer(view, chip),
    }
}

/// Says that a database saved with zeros keeps them, and how to map the
/// image into it: in the UI from the menu; headless with the command, run
/// from a script, then saving the database.
fn database_note(chip: Chip, ui: bool) {
    let chip = chip.display_name();
    if ui {
        tracing::info!(
            "this database was saved without the {chip} ROM image, and keeps its zero-filled ROM region; \
             Plugins > Espressif > Download ROM Images maps the image into it"
        );
    } else {
        tracing::warn!(
            "this database was saved without the {chip} ROM image, and keeps its zero-filled ROM region. \
             To map the image into it, {}",
            headless_database_remedy()
        );
    }
}

/// How to map a ROM image into a database headless.
fn headless_database_remedy() -> String {
    format!(
        "run the plugin command '{COMMAND}' on the view (in Python: \
         ctx = binaryninja.PluginCommandContext(bv); \
         binaryninja.PluginCommand.get_valid_list(ctx)[{COMMAND:?}].execute(ctx)), wait for it, and save \
         the database; or analyze the original file again"
    )
}

/// Registers the analysis step that maps ROM images into zero-filled ROM
/// code regions (and offers the download). It runs on every analysis
/// update, at the start of module analysis, after the ELF fix-ups.
pub fn register_activity() {
    let config = activity::Config::action(
        "espressif.rom.images",
        "Map Espressif ROM images",
        "Fills the mask ROM's code region of ESP firmware images and ELF files of Espressif chips with Espressif's ROM image, once one is available.",
    )
    .eligibility(activity::Eligibility::auto());
    let step = Activity::new_with_action(config, |ctx: &AnalysisContext| {
        refresh(&ctx.view());
    });
    let registered = Workflow::cloned("core.module.metaAnalysis")
        .ok_or(())
        .and_then(|w| w.activity_before(&step, crate::elf::FIRST_STEP))
        .and_then(|w| w.register());
    if registered.is_err() {
        tracing::error!("cannot register the Espressif ROM image step");
    }
}

// ---------------------------------------------------------------------------
// Consent

fn declined_path() -> PathBuf {
    cache::dir("rom-elfs.json")
}

/// The SHA-256 of the release whose download the user declined, if any.
fn declined() -> Option<String> {
    let text = std::fs::read_to_string(declined_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("declined")?.as_str().map(str::to_string)
}

fn set_declined(sha256: Option<&str>) {
    let path = declined_path();
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("json.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::json!({ "declined": sha256 }).to_string())?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(err) = write() {
        tracing::warn!("cannot write {}: {err}", path.display());
    }
}

fn auto_download() -> bool {
    Settings::global().get_bool(AUTO_DOWNLOAD)
}

fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1e6)
}

/// The text of the consent prompt.
pub fn prompt_text(chip: Chip, release: &Release, cache: &std::path::Path) -> String {
    format!(
        "The {chip} mask ROM's code is not available, so its region holds only the ROM's typed \
         functions, over zeros.\n\n\
         Download Espressif's ROM ELF files (esp-rom-elfs {tag}, {size}) from {url} and map the \
         ROM's code from them?\n\n\
         Source: {SOURCE}. The ELF files are licensed under {LICENSE}; the ROM code is closed \
         source, Copyright Espressif Systems, and contains third-party components (listed at \
         {NOTICE}). The download \
         is verified against the SHA-256 hash shipped with this plugin and unpacked into {cache}.\n\n\
         If you answer No, you won't be asked again for this release; use Plugins > Espressif > \
         Download ROM Images later, or set '{ELF_DIRECTORY}' to a directory of ROM ELF files \
         (ESP-IDF's are found by themselves).",
        chip = chip.display_name(),
        tag = release.tag,
        size = mb(release.size),
        url = release.url,
        cache = cache.display(),
    )
}

/// How to get a ROM image, for a view that has none: in the UI, from the
/// menu or the settings; headless, from the settings (`autoDownload` is
/// global: as a load option it is ignored), then analyzing the file again,
/// or from the command, run from a script.
pub fn explain_text(chip: Chip, ui: bool, declined: bool) -> String {
    let chip = chip.display_name();
    if ui {
        return format!(
            "no {chip} ROM image is available, so the ROM's code region holds zeros. Set '{ELF_DIRECTORY}' \
             to a directory of Espressif's ROM ELF files, or let the plugin download them from {SOURCE} \
             (Plugins > Espressif > Download ROM Images, or the '{AUTO_DOWNLOAD}' setting){}",
            if declined {
                "; you declined the download before"
            } else {
                ""
            }
        );
    }
    format!(
        "no {chip} ROM image is available, so the ROM's code region holds zeros (headless, the plugin \
         never asks to download it). Set '{ELF_DIRECTORY}' to a directory of Espressif's ROM ELF files \
         (in the user settings, or as a load option), or '{AUTO_DOWNLOAD}' to true to download them \
         from {SOURCE}, in the user settings (settings.json in the user directory, or \
         binaryninja.Settings() before loading; not as a load option, which this global setting \
         ignores), and analyze the file again. A database keeps the ROM region it was saved with: to \
         map the image into one, {}.",
        headless_database_remedy()
    )
}

/// Runs `f` on the UI thread and returns its result.
fn on_main_thread<T: Send + 'static>(f: impl Fn() -> T + Send + Sync + 'static) -> Option<T> {
    let result = std::sync::Arc::new(Mutex::new(None));
    let out = result.clone();
    binaryninja::main_thread::execute_on_main_thread_and_wait(move || {
        *out.lock().unwrap() = Some(f());
    });
    result.lock().unwrap().take()
}

fn message(text: String, icon: MessageBoxIcon) {
    if binaryninja::is_ui_enabled() {
        on_main_thread(move || {
            show_message_box(TITLE, &text, MessageBoxButtonSet::OKButtonSet, icon);
        });
    }
}

fn ask(text: String) -> bool {
    if !binaryninja::is_ui_enabled() {
        return false;
    }
    on_main_thread(move || {
        show_message_box(
            TITLE,
            &text,
            MessageBoxButtonSet::YesNoButtonSet,
            MessageBoxIcon::QuestionIcon,
        )
    }) == Some(MessageBoxButtonResult::YesButton)
}

/// What to do when a view has no ROM image to map, by [`offer`]'s rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// In the background (the UI).
    Download,
    /// Headless, while the view loads (see [`download_inline`]).
    DownloadInline,
    Ask,
    /// Log how to get one.
    Explain,
}

/// [`offer`]'s rules: with `autoDownload`, download, in the background in
/// the UI and while the view loads headless; in the UI, ask unless the user
/// declined this release; otherwise (headless, or declined), explain. Each
/// happens at most once per session (see [`once`], and for a headless
/// download [`plan_inline`]).
pub fn plan_offer(ui: bool, auto: bool, declined: bool) -> Offer {
    match () {
        _ if auto && ui => Offer::Download,
        _ if auto => Offer::DownloadInline,
        _ if ui && !declined => Offer::Ask,
        _ => Offer::Explain,
    }
}

/// Whether `offer` is to happen now: the first time in the session, or for
/// a download, also a while after the last one failed (see
/// [`AutoDownload`]).
fn once(offer: Offer) -> bool {
    let flag = match offer {
        Offer::Download => {
            let mut state = lock(&AUTO_DOWNLOAD_STATE);
            let start = state.may_start(Instant::now());
            if start {
                *state = AutoDownload::Started;
            }
            return start;
        }
        // `rom_image_for_load` tried it already, and reported a failure.
        Offer::DownloadInline => return false,
        Offer::Ask => &ASKED,
        Offer::Explain => &EXPLAINED,
    };
    !flag.swap(true, Ordering::SeqCst)
}

/// Offers to download the ROM images for a view that has none, by
/// [`plan_offer`]'s rules. Never blocks: asking and downloading happen on a
/// thread of their own. (Headless, [`rom_image_for_load`] has downloaded
/// already, if it was to.)
fn offer(view: &BinaryView, chip: Chip) {
    let declined = declined().is_some_and(|d| d.eq_ignore_ascii_case(RELEASE.sha256));
    let plan = plan_offer(binaryninja::is_ui_enabled(), auto_download(), declined);
    if !once(plan) {
        return;
    }
    if plan == Offer::Explain {
        let ui = binaryninja::is_ui_enabled();
        let text = explain_text(chip, ui, declined);
        // Headless, nobody was asked, and this is all that tells that the
        // ROM's code is missing.
        if ui {
            tracing::info!("{text}");
        } else {
            tracing::warn!("{text}");
        }
        return;
    }
    let view = view.to_owned();
    std::thread::spawn(move || {
        if plan == Offer::Ask && !ask(prompt_text(chip, &RELEASE, &cache_dir())) {
            set_declined(Some(RELEASE.sha256));
            tracing::info!(
                "ROM image download declined; download them later with Plugins > Espressif > Download ROM Images"
            );
            return;
        }
        // An automatic download that failed is retried later (see
        // `AutoDownload`); only its first failure is reported, the retries'
        // are only logged.
        let failed_before = plan == Offer::Download && AUTO_DOWNLOAD_FAILED.load(Ordering::SeqCst);
        rom_status::record_status(&view, chip, rom_status::Status::Downloading);
        let downloaded = download_release(!failed_before).is_ok();
        rom_status::record_unmapped(&view, chip);
        if plan == Offer::Download && !downloaded {
            failed();
        }
        // In the UI, the view that wanted the image maps it now. (Headless,
        // the view's load downloads, see `rom_image_for_load`.)
        if downloaded && binaryninja::is_ui_enabled() {
            map_into(&view, chip);
        }
    });
}

// ---------------------------------------------------------------------------
// Download

const LOOPBACK: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// The URL to download the release from: the release's, or the test
/// override, which must be on a loopback address.
pub fn download_url(release: &Release, test_url: Option<&str>) -> Result<String, String> {
    let Some(url) = test_url.filter(|u| !u.is_empty()) else {
        if !release.url.starts_with("https://") {
            return Err(format!("{} is not an HTTPS URL", release.url));
        }
        return Ok(release.url.to_string());
    };
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| format!("the test URL {url} is not HTTP"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if authority.starts_with('[') {
        authority.split_inclusive(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if authority.contains('@') || !LOOPBACK.contains(&host.to_ascii_lowercase().as_str()) {
        return Err(format!("the test URL {url} must be on a loopback address"));
    }
    Ok(url.to_string())
}

fn fetch(url: &str, task: &BackgroundTask, cancel: Arc<AtomicBool>) -> Result<Vec<u8>, String> {
    let provider = DownloadProvider::try_default()
        .map_err(|_| "Binary Ninja has no download provider".to_string())?;
    let mut instance = provider
        .create_instance()
        .map_err(|_| "cannot create a download".to_string())?;
    let data = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = data.clone();
    let progress_task = task.to_owned();
    let progress_cancel = cancel.clone();
    let callbacks = DownloadInstanceInputOutputCallbacks {
        read: None,
        write: Some(Box::new(move |chunk: &[u8]| {
            let mut data = sink.borrow_mut();
            if data.len() + chunk.len() > MAX_DOWNLOAD {
                return 0; // too large: stop
            }
            data.extend_from_slice(chunk);
            chunk.len()
        })),
        progress: Some(Box::new(move |done: usize, total: usize| {
            if let Some(percent) = (done * 100).checked_div(total) {
                progress_task
                    .set_progress_text(&format!("Espressif: downloading ROM images {percent}%"));
            }
            !progress_task.is_cancelled() && !progress_cancel.load(Ordering::SeqCst)
        })),
    };
    let headers = [("User-Agent".to_string(), "binja-xtensa".to_string())];
    let response = instance.perform_custom_request("GET", url, headers, &callbacks)?;
    drop(callbacks);
    if task.is_cancelled() || cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    if !(200..300).contains(&response.status_code) {
        return Err(format!("the server answered {}", response.status_code));
    }
    let data = data.take();
    Ok(data)
}

/// Downloads the pinned release, verifies it and unpacks its ROM ELF files
/// into the cache, with a background task to show for it. The caller has
/// the user's consent. A failure is logged, and also shown in the UI if
/// `report` is set.
///
/// If another download is running, this waits for it, and does not
/// download again if it succeeded: the caller then has its files. One that
/// was cancelled (a headless load gave up on it) is not waited for.
pub fn download_release(report: bool) -> Result<usize, String> {
    download_release_cancellable(report, Arc::default())
}

/// [`download_release`], given up (as a failure) once `cancel` is set.
fn download_release_cancellable(report: bool, cancel: Arc<AtomicBool>) -> Result<usize, String> {
    let test_url = std::env::var(TEST_URL_ENV).ok();
    let url = download_url(&RELEASE, test_url.as_deref())?;
    {
        let mut downloads = lock(&DOWNLOADS);
        if downloads.busy() {
            tracing::info!("waiting for the ROM image download that is running");
            // Woken when it ends, or checked again when it is cancelled.
            while downloads.busy() {
                downloads = DOWNLOAD_ENDED
                    .wait_timeout(downloads, Duration::from_millis(500))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            if let Some(n) = downloads.last {
                return Ok(n);
            }
        }
        downloads.running = Some(cancel.clone());
        downloads.last = None;
    }
    let task = BackgroundTask::new("Espressif: downloading ROM images", true);
    tracing::info!(
        "downloading esp-rom-elfs {} ({}) from {url}",
        RELEASE.tag,
        mb(RELEASE.size)
    );
    let dest = cache_dir();
    let result = fetch(&url, &task, cancel.clone())
        .and_then(|archive| rom_images::unpack(&archive, &RELEASE, &dest));
    task.finish();
    {
        let mut downloads = lock(&DOWNLOADS);
        // Unless it was cancelled, and another started since.
        if downloads
            .running
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, &cancel))
        {
            downloads.running = None;
            downloads.last = result.as_ref().ok().copied();
            DOWNLOAD_FAILED.store(result.is_err(), Ordering::SeqCst);
        }
    }
    DOWNLOAD_ENDED.notify_all();
    match &result {
        Ok(n) => {
            set_declined(None);
            let text = format!(
                "Downloaded Espressif's ROM images (esp-rom-elfs {}, verified SHA-256 {}): {n} ROM ELF files in {}. {}",
                RELEASE.tag,
                RELEASE.sha256,
                dest.display(),
                if binaryninja::is_ui_enabled() {
                    "Views map them when they are next analyzed or reopened.".to_string()
                } else {
                    format!(
                        "Views loaded from now on map them, and views loaded before when they are next \
                         analyzed. A database keeps the ROM region it was saved with: to map the image into \
                         one, {}.",
                        headless_database_remedy()
                    )
                }
            );
            tracing::info!("{text}");
        }
        Err(err) => {
            let text = format!(
                "Espressif's ROM images were not downloaded from {url}: {err}. {}",
                if binaryninja::is_ui_enabled() {
                    "Plugins > Espressif > Download ROM Images tries again.".to_string()
                } else {
                    format!("The plugin command '{COMMAND}' tries again.")
                }
            );
            tracing::error!("{text}");
            if report {
                message(text, MessageBoxIcon::ErrorIcon);
            }
        }
    }
    result
}

/// Records that the automatic download failed (see [`AutoDownload`]).
fn failed() {
    AUTO_DOWNLOAD_FAILED.store(true, Ordering::SeqCst);
    DOWNLOAD_FAILED.store(true, Ordering::SeqCst);
    *lock(&AUTO_DOWNLOAD_STATE) = AutoDownload::Failed(Instant::now());
}

/// How long a headless load waits for the download.
fn inline_timeout() -> Duration {
    std::env::var(TEST_URL_ENV)
        .ok()
        .filter(|u| !u.is_empty())
        .and_then(|_| std::env::var(TEST_TIMEOUT_ENV).ok()?.parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(INLINE_TIMEOUT)
}

/// Headless, with [`AUTO_DOWNLOAD`] on: downloads the ROM images (the
/// setting is the consent) and waits for them, at most [`inline_timeout`],
/// so that the view being loaded maps the image itself. Returns whether the
/// download succeeded.
///
/// The download runs on a thread of its own, once per session (see
/// [`plan_inline`]): loads while it runs wait for it, and once it is done
/// none starts again unless it failed, a while ago (see [`AutoDownload`]).
/// One that times out is cancelled, and fails; the view loads with zeros,
/// and a warning says so.
fn download_inline(chip: Chip) -> bool {
    let timeout = inline_timeout();
    let inline = {
        let mut state = lock(&AUTO_DOWNLOAD_STATE);
        let mut current = lock(&INLINE);
        let running = current
            .as_ref()
            .filter(|i| lock(&i.done).is_none())
            .map(|i| i.cancel.load(Ordering::SeqCst));
        match (
            plan_inline(*state, running, Instant::now()),
            current.clone(),
        ) {
            (InlinePlan::Skip, _) => return false,
            (InlinePlan::Wait, Some(inline)) => {
                tracing::info!("waiting for the ROM image download that is running");
                inline
            }
            _ => {
                *state = AutoDownload::Started;
                let inline = Arc::new(Inline::default());
                *current = Some(inline.clone());
                let download = inline.clone();
                std::thread::spawn(move || {
                    let ok = download_release_cancellable(false, download.cancel.clone()).is_ok();
                    // A download that timed out failed then.
                    if !ok && !download.cancel.load(Ordering::SeqCst) {
                        failed();
                    }
                    *lock(&download.done) = Some(ok);
                    download.finished.notify_all();
                });
                inline
            }
        }
    };
    let done = lock(&inline.done);
    let (done, _) = inline
        .finished
        .wait_timeout_while(done, timeout, |d| d.is_none())
        .unwrap_or_else(|e| e.into_inner());
    let outcome = *done;
    drop(done);
    let why = match outcome {
        Some(true) => return true,
        Some(false) => "the download failed".to_string(),
        None => {
            if !inline.cancel.swap(true, Ordering::SeqCst) {
                failed();
            }
            format!("the download did not finish in {} s", timeout.as_secs_f64())
        }
    };
    // Once, however many loads waited for it.
    if inline.warned.swap(true, Ordering::SeqCst) {
        return false;
    }
    tracing::warn!(
        "no {} ROM image: {why}, so the ROM's code region holds zeros. An analysis of a view without \
         one tries again in {} minutes; or set '{ELF_DIRECTORY}' to a directory of Espressif's ROM ELF \
         files in the user settings, and analyze the file again.",
        chip.display_name(),
        RETRY_AFTER.as_secs() / 60
    );
    false
}

/// Maps the chip's ROM image into the view's zero-filled ROM code regions,
/// database or not, and says what it did. A view that has the image
/// already gets the image's functions it lacks (see [`CREATE_FUNCTIONS`]).
fn map_into(view: &BinaryView, chip: Chip) {
    let Some(found) = rom_image(view, chip) else {
        let text = format!(
            "No {} ROM image was found (is '{LOAD_IMAGES}' off?).",
            chip.display_name()
        );
        tracing::warn!("{text}");
        message(text, MessageBoxIcon::WarningIcon);
        rom_status::record_unmapped(view, chip);
        return;
    };
    let text = if rom_types::holds_image(view, chip, &found.elf) {
        // A view (a database, say) that holds this image's bytes but not
        // all its functions, or not its data: one saved by a version that
        // only named them or did not map the data, or with
        // `CREATE_FUNCTIONS` off then. Only the bytes say it is this image:
        // another file of the same name may differ. A file with the same
        // bytes may name more functions: they are named too.
        let data = rom_types::add_rom_data_region(view, chip, &found.elf);
        rom_status::sync_data_alias(view);
        let created = if create_functions(view) {
            rom_types::name_image_functions(view, &found.elf);
            rom_types::create_image_functions(view, &found.elf)
        } else {
            0
        };
        if data || created > 0 {
            view.update_analysis();
        }
        format!(
            "This view has the {} ROM image of {}; created {created} more of its functions{}.",
            chip.display_name(),
            found.path.display(),
            if data { " and mapped its data" } else { "" }
        )
    } else if !rom_types::has_zero_filled_region(view, chip) {
        format!(
            "This view needs no {} ROM image: it has one, or maps the ROM itself.",
            chip.display_name()
        )
    } else if rom_types::map_rom_image(view, chip, &found) {
        view.update_analysis();
        format!(
            "Mapped the {} ROM from {} into this view.",
            chip.display_name(),
            found.path.display()
        )
    } else {
        format!(
            "Could not map the {} ROM into this view.",
            chip.display_name()
        )
    };
    tracing::info!("{text}");
    message(text, MessageBoxIcon::InformationIcon);
}

/// *Plugins > Espressif > Download ROM Images*: downloads the ROM images
/// (asking is the consent) and maps the view's chip's ROM.
struct DownloadCommand;

impl Command for DownloadCommand {
    fn action(&self, view: &BinaryView) {
        let Some(chip) = view_chip(view) else {
            return;
        };
        let view: Ref<BinaryView> = view.to_owned();
        std::thread::spawn(move || {
            if rom_image(&view, chip).is_none() {
                rom_status::record_status(&view, chip, rom_status::Status::Downloading);
                let downloaded = download_release(true);
                rom_status::record_unmapped(&view, chip);
                if downloaded.is_err() {
                    return;
                }
            } else {
                tracing::info!("an {} ROM image is already available", chip.display_name());
            }
            map_into(&view, chip);
        });
    }

    fn valid(&self, view: &BinaryView) -> bool {
        view_chip(view).is_some()
    }
}

pub fn register_command() {
    command::register_command(
        COMMAND,
        "Download Espressif's ROM images (esp-rom-elfs, from GitHub) and map the chip's ROM code into this view",
        DownloadCommand,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_rules() {
        use Offer::*;
        // The setting downloads without asking, in the UI and headless,
        // even after a "No": in the background in the UI, while the view
        // loads headless.
        assert_eq!(plan_offer(true, true, true), Download);
        assert_eq!(plan_offer(true, true, false), Download);
        assert_eq!(plan_offer(false, true, false), DownloadInline);
        assert_eq!(plan_offer(false, true, true), DownloadInline);
        // The UI asks, unless declined; headless never asks.
        assert_eq!(plan_offer(true, false, false), Ask);
        assert_eq!(plan_offer(true, false, true), Explain);
        assert_eq!(plan_offer(false, false, false), Explain);
    }

    #[test]
    fn automatic_download_retries() {
        use AutoDownload::*;
        let now = Instant::now();
        assert!(NotTried.may_start(now));
        assert!(!Started.may_start(now));
        assert!(!Failed(now).may_start(now));
        assert!(!Failed(now).may_start(now + RETRY_AFTER / 2));
        assert!(Failed(now).may_start(now + RETRY_AFTER));
    }

    #[test]
    fn headless_loads_wait_for_one_download() {
        use AutoDownload::*;
        use InlinePlan::*;
        let now = Instant::now();
        // The first load starts it; the loads while it runs wait for it.
        assert_eq!(plan_inline(NotTried, None, now), Start);
        assert_eq!(plan_inline(Started, Some(false), now), Wait);
        // One that timed out is not waited for again, but retried a while
        // later, even if it is stuck.
        assert_eq!(plan_inline(Failed(now), Some(true), now), Skip);
        assert_eq!(
            plan_inline(Failed(now), Some(true), now + RETRY_AFTER),
            Start
        );
        // Done: not again, unless it failed a while ago.
        assert_eq!(plan_inline(Started, None, now), Skip);
        assert_eq!(plan_inline(Failed(now), None, now), Skip);
        assert_eq!(plan_inline(Failed(now), None, now + RETRY_AFTER), Start);
    }

    #[test]
    fn headless_hints_name_headless_remedies() {
        let text = explain_text(Chip::Esp32S3, false, false);
        for needle in [
            ELF_DIRECTORY,
            AUTO_DOWNLOAD,
            "user settings",
            "load option",
            COMMAND,
            "save",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        assert!(!text.contains("Plugins >"), "{text}");
        assert!(explain_text(Chip::Esp32, true, false).contains("Plugins > Espressif"));
        assert!(explain_text(Chip::Esp32, true, true).contains("declined"));
    }

    #[test]
    fn revisions() {
        assert_eq!(parse_revision("v3.0"), Some(300));
        assert_eq!(parse_revision(" 3.0 "), Some(300));
        assert_eq!(parse_revision("V0.3"), Some(3));
        assert_eq!(parse_revision("1.1"), Some(101));
        assert_eq!(parse_revision("300"), Some(300));
        assert_eq!(parse_revision("0"), Some(0));
        for bad in ["", "v", "-1", "3.", ".3", "3.100", "abc", "3.0.1", "99999"] {
            assert_eq!(parse_revision(bad), None, "{bad}");
        }
    }

    #[test]
    fn download_urls() {
        assert_eq!(download_url(&RELEASE, None).unwrap(), RELEASE.url);
        assert_eq!(download_url(&RELEASE, Some("")).unwrap(), RELEASE.url);
        let http = Release {
            url: "http://github.com/a.tar.gz",
            ..RELEASE
        };
        assert!(download_url(&http, None).is_err());
        for ok in [
            "http://127.0.0.1:8000/a.tar.gz",
            "http://localhost/a.tar.gz",
            "https://[::1]:8443/a.tar.gz",
        ] {
            assert_eq!(download_url(&RELEASE, Some(ok)).unwrap(), ok);
        }
        for bad in [
            "http://example.com/a.tar.gz",
            "http://127.0.0.1.example.com/a.tar.gz",
            "http://user@example.com@127.0.0.1/a.tar.gz",
            "http://127.0.0.1@example.com/a.tar.gz",
            "ftp://127.0.0.1/a.tar.gz",
            "file:///tmp/a.tar.gz",
        ] {
            assert!(download_url(&RELEASE, Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_prompt_names_the_source_and_licence() {
        let text = prompt_text(Chip::Esp32, &RELEASE, std::path::Path::new("/cache"));
        for needle in [
            SOURCE,
            LICENSE,
            NOTICE,
            RELEASE.url,
            "third-party",
            "/cache",
            "4.9 MB",
        ] {
            assert!(text.contains(needle), "{needle}");
        }
    }
}
