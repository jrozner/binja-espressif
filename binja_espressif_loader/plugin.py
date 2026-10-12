"""Binary Ninja side of the loader: settings, UI actions, prompts and the
background task. The decisions are made in core.py.

The native plugin is not loaded from Python: it declares plugin
dependencies (view_elf, arch_riscv, svd_ninja) that only Binary Ninja's
native plugin loading honours, so the library is copied into the user
plugin directory and Binary Ninja loads it at the next start.
"""

import os
import threading
import traceback
from typing import Optional

import binaryninja
from binaryninja import core_ui_enabled, interaction, mainthread, user_directory, user_plugin_path
from binaryninja import _binaryninjacore as bncore
from binaryninja.enums import MessageBoxButtonResult, MessageBoxButtonSet, MessageBoxIcon
from binaryninja.log import Logger
from binaryninja.plugin import BackgroundTaskThread
from binaryninja.settings import Settings

from . import core

LOG = Logger(0, "Espressif.Loader")

PACKAGE_DIR = os.path.dirname(os.path.abspath(__file__))
PLUGIN_ROOT = os.path.dirname(PACKAGE_DIR)
MANIFEST = os.path.join(PACKAGE_DIR, "manifest.json")
PLUGIN_JSON = os.path.join(PLUGIN_ROOT, "plugin.json")

AUTO_DOWNLOAD = "espressif.loader.autoDownload"
TITLE = "Espressif (binja-espressif)"
INSTALL_ACTION = "Espressif\\Install or Update Native Library"
REMOVE_ACTION = "Espressif\\Remove Native Library"
# Only for tests/test_headless.py: fetch from a local server instead of the
# GitHub release. The download is still verified against manifest.json.
TEST_BASE_URL_ENV = "BN_ESPRESSIF_LOADER_TEST_BASE_URL"

_busy = threading.Lock()


class Facts:
    """What the running Binary Ninja and the plugin directory look like."""

    def __init__(self):
        self.manifest = core.load_manifest(MANIFEST)
        plugin_version = core.read_plugin_version(PLUGIN_JSON)
        if plugin_version != self.manifest.version:
            raise core.ManifestError(
                "manifest.json is for version {} but plugin.json is version {}: this is not a release "
                "checkout, so no prebuilt library is installed. {}".format(
                    self.manifest.version, plugin_version, core.BUILD_FROM_SOURCE))
        self.abi_max = int(bncore.BNGetCurrentCoreABIVersion())
        get_min = getattr(bncore, "BNGetMinimumCoreABIVersion", None)
        self.abi_min = int(get_min()) if get_min is not None else self.abi_max
        self.platform = core.host_platform()
        self.asset = core.select_asset(self.manifest, self.platform, self.abi_min, self.abi_max)
        plugin_dir = user_plugin_path()
        if not plugin_dir:
            raise core.LoaderError("Binary Ninja reports no user plugin directory")
        self.plugin_dir = plugin_dir
        self.own_name = core.installed_name(self.platform) if self.platform else None
        self.own_path = os.path.join(plugin_dir, self.own_name) if self.own_name else None
        self.other_copies = core.find_other_copies(plugin_dir, self.own_name or "")
        self.state = core.installed_state(self.own_path, self.asset) if self.asset else None
        self.native_loaded = native_loaded()

    def unsupported_message(self) -> str:
        return core.unsupported_message(self.manifest, self.platform, self.abi_min, self.abi_max)


def native_loaded() -> bool:
    try:
        binaryninja.Architecture[core.NATIVE_ARCHITECTURE]
        return True
    except Exception:
        return False


def declined_path() -> str:
    return os.path.join(user_directory() or PLUGIN_ROOT, "espressif", "loader.json")


def auto_download_enabled() -> bool:
    try:
        return Settings().get_bool(AUTO_DOWNLOAD)
    except Exception:
        return False


def register_settings() -> None:
    settings = Settings()
    # The native plugin registers the same group; registering it again is harmless.
    settings.register_group("espressif", "Espressif")
    settings.register_setting(AUTO_DOWNLOAD, """{
        "title": "Download the native library automatically",
        "type": "boolean",
        "default": false,
        "description": "Let binja-espressif's Python loader download its prebuilt native library from the plugin's GitHub release, without asking, when it is missing or out of date (the download is verified against SHA-256 hashes shipped with the plugin). This is the only way it downloads in headless Binary Ninja. Restart Binary Ninja afterwards to load the library.",
        "ignore": ["SettingsProjectScope", "SettingsResourceScope"]
    }""")


# ---------------------------------------------------------------------------
# UI helpers (no-ops headless)


def _on_main_thread(func):
    """Runs func on the UI thread and returns its result."""
    result = []
    mainthread.execute_on_main_thread_and_wait(lambda: result.append(func()))
    return result[0] if result else None


def _message(text: str, icon=MessageBoxIcon.InformationIcon) -> None:
    if core_ui_enabled():
        _on_main_thread(lambda: interaction.show_message_box(
            TITLE, text, MessageBoxButtonSet.OKButtonSet, icon))


def _ask(text: str) -> bool:
    if not core_ui_enabled():
        return False
    answer = _on_main_thread(lambda: interaction.show_message_box(
        TITLE, text, MessageBoxButtonSet.YesNoButtonSet, MessageBoxIcon.QuestionIcon))
    return answer == MessageBoxButtonResult.YesButton


def _size(n: int) -> str:
    return "{:.1f} MB".format(n / 1e6)


# ---------------------------------------------------------------------------
# Work


def _download_and_install(facts: Facts, task: Optional[BackgroundTaskThread]) -> bool:
    asset = facts.asset
    test_base_url = os.environ.get(TEST_BASE_URL_ENV) or None
    url = core.asset_url(facts.manifest, asset, test_base_url)
    LOG.log_info("Downloading {} ({}) from {}".format(asset.name, _size(asset.size), url))

    def progress(done, total):
        if task is not None:
            if task.cancelled:
                raise core.DownloadError("cancelled")
            task.progress = "Espressif: downloading native library {}%".format(done * 100 // max(total, 1))

    try:
        path = core.install(facts.manifest, asset, facts.plugin_dir, test_base_url=test_base_url, progress=progress)
    except core.LoaderError as e:
        LOG.log_error("Not installed: {}".format(e))
        _message("The native library was not installed:\n\n{}".format(e), MessageBoxIcon.ErrorIcon)
        return False
    except OSError as e:
        LOG.log_error("Installing into {} failed: {}".format(facts.plugin_dir, e))
        _message("Installing the native library into {} failed:\n\n{}".format(facts.plugin_dir, e),
                 MessageBoxIcon.ErrorIcon)
        return False
    try:
        core.write_declined(declined_path(), None)
    except OSError:
        pass
    text = ("Installed binja-espressif's native library {} (verified SHA-256 {}) as {}. Restart Binary Ninja "
            "to load it.".format(asset.name, asset.sha256, path))
    LOG.log_warn(text)
    _message(text)
    return True


def _prompt_text(facts: Facts) -> str:
    asset = facts.asset
    what = "is not installed" if facts.state is core.State.MISSING else "is out of date"
    return ("binja-espressif's native library (the Xtensa architectures and the Espressif loaders) {}.\n\n"
            "Download {} ({}) from {} and install it into {}?\n\n"
            "The download is verified against the SHA-256 hash shipped with this plugin. Binary Ninja must be "
            "restarted afterwards. If you answer No, you won't be asked again for this version; use Plugins > "
            "Espressif > Install or Update Native Library later.".format(
                what, asset.name, _size(asset.size), core.asset_url(facts.manifest, asset), facts.plugin_dir))


def startup_check(task: Optional[BackgroundTaskThread] = None) -> None:
    if not _busy.acquire(blocking=False):
        return
    try:
        facts = Facts()
        core.cleanup_stale(facts.plugin_dir)
        if facts.asset is None:
            if facts.native_loaded:
                return
            LOG.log_warn(facts.unsupported_message())
            return
        ui = core_ui_enabled()
        plan = core.plan_startup(
            asset=facts.asset, state=facts.state, other_copies=facts.other_copies,
            native_loaded=facts.native_loaded, ui=ui, auto_download=auto_download_enabled(),
            declined=core.read_declined(declined_path()), setting=AUTO_DOWNLOAD, plugin_dir=facts.plugin_dir)
        if plan.message:
            LOG.log_warn(plan.message)
        if plan.action is core.Action.DOWNLOAD:
            _download_and_install(facts, task)
        elif plan.action is core.Action.PROMPT:
            if _ask(_prompt_text(facts)):
                _download_and_install(facts, task)
            else:
                core.write_declined(declined_path(), facts.asset.sha256)
                LOG.log_info("Native library download declined; install it later with Plugins > Espressif > "
                             "Install or Update Native Library.")
    except core.LoaderError as e:
        LOG.log_warn(str(e))
    except Exception:
        LOG.log_error("Startup check failed:\n" + traceback.format_exc())
    finally:
        _busy.release()


def install_command(task: Optional[BackgroundTaskThread] = None) -> None:
    """Installs or updates the native library on request (that request is the consent)."""
    if not _busy.acquire(blocking=False):
        _message("The native library is already being checked or installed.")
        return
    try:
        facts = Facts()
        if facts.asset is None:
            text = facts.unsupported_message()
            LOG.log_warn(text)
            _message(text, MessageBoxIcon.WarningIcon)
        elif facts.other_copies:
            text = ("binja-espressif's native library is already present in {} as {}. Not installing a second "
                    "copy, which would register the same architectures twice; delete it and restart Binary "
                    "Ninja first.".format(facts.plugin_dir, ", ".join(facts.other_copies)))
            LOG.log_warn(text)
            _message(text, MessageBoxIcon.WarningIcon)
        elif facts.state is core.State.CURRENT:
            text = "The native library {} is installed and up to date ({}).".format(facts.asset.name, facts.own_path)
            if not facts.native_loaded:
                text += " It is not loaded yet: restart Binary Ninja."
            LOG.log_info(text)
            _message(text)
        else:
            _download_and_install(facts, task)
    except core.LoaderError as e:
        LOG.log_warn(str(e))
        _message(str(e), MessageBoxIcon.WarningIcon)
    except Exception:
        LOG.log_error("Installing the native library failed:\n" + traceback.format_exc())
    finally:
        _busy.release()


def remove_command() -> None:
    try:
        platform = core.host_platform()
        plugin_dir = user_plugin_path()
        if platform is None or not plugin_dir:
            _message("No prebuilt native library is installed on this platform.")
            return
        path = os.path.join(plugin_dir, core.installed_name(platform))
        if not os.path.lexists(path):
            _message("The prebuilt native library is not installed ({} does not exist). A library you built "
                     "yourself has to be removed by hand.".format(path))
            return
        if not _ask("Remove binja-espressif's prebuilt native library {}? Restart Binary Ninja afterwards.".format(path)):
            return
        core.uninstall(plugin_dir, platform)
        # Don't offer it again at the next start, nor download it again.
        try:
            facts = Facts()
            if facts.asset is not None:
                core.write_declined(declined_path(), facts.asset.sha256)
        except Exception:
            pass
        text = "Removed {}. Restart Binary Ninja to unload it.".format(path)
        if auto_download_enabled():
            try:
                Settings().set_bool(AUTO_DOWNLOAD, False)
            except Exception:
                pass
            # set_bool reports failure by returning False; read the setting
            # back rather than trust that, since it is what the next start sees.
            if auto_download_enabled():
                text += (" The '{}' setting is on and turning it off failed, so the next start downloads the "
                         "library again: turn it off in Settings first.".format(AUTO_DOWNLOAD))
                LOG.log_warn(text)
                _message(text, MessageBoxIcon.WarningIcon)
                return
            text += " The '{}' setting was on; it is now off, so the library is not downloaded again.".format(
                AUTO_DOWNLOAD)
        LOG.log_info(text)
        _message(text)
    except Exception:
        LOG.log_error("Removing the native library failed:\n" + traceback.format_exc())


class _Task(BackgroundTaskThread):
    def __init__(self, text, func):
        super().__init__(text, True)
        self.func = func

    def run(self):
        self.func(self)


def _register_ui_actions() -> None:
    from binaryninjaui import Menu, UIAction, UIActionHandler

    def install(_context):
        _Task("Espressif: installing native library", install_command).start()

    def remove(_context):
        # Not on the UI thread: remove_command waits for the UI thread to answer.
        threading.Thread(target=remove_command, daemon=True).start()

    for name, func in ((INSTALL_ACTION, install), (REMOVE_ACTION, remove)):
        UIAction.registerAction(name)
        UIActionHandler.globalActions().bindAction(name, UIAction(func))
        Menu.mainMenu("Plugins").addAction(name, "Espressif")


def start() -> None:
    """Called once when Binary Ninja loads the Python plugin; never raises."""
    try:
        register_settings()
    except Exception:
        LOG.log_error("Registering the loader's settings failed:\n" + traceback.format_exc())
    if core_ui_enabled():
        try:
            _register_ui_actions()
        except Exception:
            LOG.log_error("Registering the loader's actions failed:\n" + traceback.format_exc())
        try:
            _Task("Espressif: checking native library", startup_check).start()
        except Exception:
            LOG.log_error("Starting the loader failed:\n" + traceback.format_exc())
    else:
        # Headless: no UI thread to block, and a script that imports
        # binaryninja expects plugin loading to be finished when it returns.
        startup_check()
