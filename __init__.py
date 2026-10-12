"""Binary Ninja entry point of binja-espressif as installed by the extension manager.

The plugin itself is a native (Rust) library; this Python plugin only
installs the prebuilt library for the running Binary Ninja, see
binja_espressif_loader/. A self-built library in the plugin directory works
without it.
"""

try:
    from .binja_espressif_loader import plugin as _plugin

    _plugin.start()
except Exception:  # never break Binary Ninja's startup
    try:
        import traceback

        from binaryninja.log import log_error

        log_error("binja-espressif loader failed to start:\n" + traceback.format_exc(), "Espressif.Loader")
    except Exception:
        pass
