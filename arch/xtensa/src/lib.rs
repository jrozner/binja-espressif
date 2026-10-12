//! Binary Ninja architecture plugin for the Xtensa cores of Espressif's
//! chips: the ESP8266 (LX106), ESP32 (LX6), ESP32-S2 and ESP32-S3 (LX7).
//! Each core is configured differently, so each gets an architecture of its
//! own (`xtensa-esp8266`, `xtensa-esp32`, `xtensa-esp32s2`,
//! `xtensa-esp32s3`): disassembly in GNU objdump syntax (decoded by the
//! `xtensa` crate), lifting of the whole ISA, the `windowed` and `call0`
//! calling conventions, zero-overhead loops, patching and ELF relocations.
//!
//! Nothing here knows about Espressif's firmware formats, memory maps or
//! ROMs; the Espressif view (`view_espressif`) looks the architectures up by
//! name.

mod architecture;
mod calling_convention;
mod encode;
mod exemplars;
mod flag;
mod intrinsic;
mod lift;
#[cfg(all(test, feature = "liftcheck"))]
mod liftcheck;
mod loops;
mod patch;
mod register;
mod relocation;
mod token;

use binaryninja::Endianness;
use binaryninja::architecture::{ArchitectureExt, CoreArchitecture, register_architecture};
use binaryninja::binary_view::BinaryViewType;
use xtensa_isa::{Features, Isa};

use crate::architecture::Xtensa;
use crate::relocation::XtensaElfRelocationHandler;

/// A supported core configuration. Espressif's Xtensa cores are each
/// configured differently, so each gets its own architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Core {
    Esp8266,
    Esp32,
    Esp32S2,
    Esp32S3,
}

impl Core {
    pub const ALL: [Core; 4] = [Core::Esp8266, Core::Esp32, Core::Esp32S2, Core::Esp32S3];

    pub fn isa(self) -> Isa {
        match self {
            Core::Esp8266 => Isa::ESP8266,
            Core::Esp32 => Isa::ESP32,
            Core::Esp32S2 => Isa::ESP32S2,
            Core::Esp32S3 => Isa::ESP32S3,
        }
    }

    /// The architecture (and standalone platform) name.
    pub fn arch_name(self) -> &'static str {
        match self {
            Core::Esp8266 => "xtensa-esp8266",
            Core::Esp32 => "xtensa-esp32",
            Core::Esp32S2 => "xtensa-esp32s2",
            Core::Esp32S3 => "xtensa-esp32s3",
        }
    }

    pub fn windowed(self) -> bool {
        self.isa().has(Features::WINDOWED)
    }
}

/// `e_machine` of Xtensa ELF files.
const EM_XTENSA: u32 = 94;

/// Registers an architecture, its calling conventions, its ELF relocation
/// handler and (on the windowed cores) the windowed function recognizer for
/// every core, the analysis step that finds the call0 functions on the
/// windowed cores, and the ESP32's architecture for Xtensa ELF files.
pub fn register() {
    for core in Core::ALL {
        let arch = register_architecture(core.arch_name(), move |custom_handle, handle| {
            Xtensa::new(core, handle, custom_handle)
        });
        let call0 = calling_convention::register(arch);
        arch.register_relocation_handler("ELF", move |custom_handle, handle| {
            XtensaElfRelocationHandler::new(core.isa(), custom_handle, handle)
        });
        if core.windowed() {
            arch.register_function_recognizer(calling_convention::WindowRecognizer { call0 });
        }
    }
    calling_convention::register_call0_step();

    // EM_XTENSA says nothing about the core configuration: ELF files get
    // the most common one's architecture, the ESP32's, and a platform
    // recognizer (the Espressif view's) can pick another.
    if let Some(elf) = BinaryViewType::by_name("ELF")
        && let Some(esp32) = CoreArchitecture::by_name(Core::Esp32.arch_name())
    {
        elf.register_arch(EM_XTENSA, Endianness::LittleEndian, &esp32);
    }
}

/// The plugin's entry point when it is built on its own (without the
/// `bundled` feature, which leaves it to the plugin that links this crate).
#[cfg(not(feature = "bundled"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginInit() -> bool {
    binaryninja::tracing_init!("Xtensa");
    register();
    true
}

/// See [`CorePluginInit`].
#[cfg(not(feature = "bundled"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginDependencies() {
    plugin_dependencies();
}

/// Declares the plugins this one needs loaded first: the ELF view, for the
/// relocation handler and the ELF architecture.
pub fn plugin_dependencies() {
    binaryninja::add_optional_plugin_dependency("view_elf");
}

/// Is `code` the start of a windowed-ABI function (`entry a1, n`)?
pub(crate) fn is_entry_insn(code: &[u8]) -> bool {
    code.len() >= 3 && code[0] == 0x36 && code[1] & 0x0f == 0x01
}
