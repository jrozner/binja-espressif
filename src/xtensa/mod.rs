//! The Xtensa architectures: decoding, lifting and calling conventions for
//! each supported core configuration. Nothing here knows about Espressif's
//! firmware formats or memory maps (see `crate::espressif`), and nothing
//! outside this module depends on its types: the Espressif view looks the
//! architectures up by name.

pub mod arch;
pub mod cc;
pub mod encode;
pub mod exemplars;
pub mod flags;
pub mod intrinsics;
pub mod lift;
#[cfg(all(test, feature = "liftcheck"))]
mod liftcheck;
pub mod loops;
pub mod patch;
pub mod registers;
pub mod reloc;
pub mod tokens;

use binaryninja::architecture::{ArchitectureExt, register_architecture};
use xtensa_isa::{Features, Isa};

use crate::xtensa::arch::Xtensa;
use crate::xtensa::reloc::XtensaElfRelocationHandler;

/// A supported core configuration. Espressif's Xtensa cores are each
/// configured differently, so each gets its own architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Core {
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

/// Registers an architecture, its calling conventions, its ELF relocation
/// handler and (on the windowed cores) the windowed function recognizer for
/// every core, and the analysis step that finds the call0 functions on the
/// windowed cores.
pub fn register() {
    for core in Core::ALL {
        let arch = register_architecture(core.arch_name(), move |custom_handle, handle| {
            Xtensa::new(core, handle, custom_handle)
        });
        let conventions = cc::register(arch);
        arch.register_relocation_handler("ELF", move |custom_handle, handle| {
            XtensaElfRelocationHandler::new(core.isa(), custom_handle, handle)
        });
        if core.windowed() {
            arch.register_function_recognizer(cc::WindowRecognizer {
                call0: conventions.call0,
            });
        }
    }
    cc::register_call0_step();
}

/// Is `code` the start of a windowed-ABI function (`entry a1, n`)?
pub fn is_entry_insn(code: &[u8]) -> bool {
    code.len() >= 3 && code[0] == 0x36 && code[1] & 0x0f == 0x01
}
