//! The Xtensa architectures: decoding, lifting and calling conventions for
//! each supported core configuration. Nothing here knows about Espressif's
//! firmware formats or memory maps (see `crate::espressif`).

pub mod arch;
pub mod cc;
pub mod exemplars;
pub mod flags;
pub mod intrinsics;
pub mod lift;
pub mod loops;
pub mod registers;
pub mod tokens;

use binaryninja::architecture::{ArchitectureExt, register_architecture};
use xtensa_isa::{Features, Isa};

use crate::xtensa::arch::Xtensa;

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

    /// The addresses of the functions found by scanning `code`, loaded at
    /// `addr`, for prologues. Every windowed-ABI function starts with a
    /// 4-byte aligned `entry`; call0 code has no prologue reliable enough to
    /// scan for, so nothing is found on the call0-only cores.
    pub fn function_starts(self, addr: u32, code: &[u8]) -> Vec<u32> {
        if !self.windowed() {
            return Vec::new();
        }
        entry_offsets(addr, code)
            .map(|off| addr + off as u32)
            .collect()
    }

    /// Does `code` start with the usual prologue of the core's functions:
    /// `entry` on the windowed cores, `addi a1, a1, -n` on the others?
    pub fn is_prologue(self, code: &[u8]) -> bool {
        if self.windowed() {
            is_entry_insn(code)
        } else {
            is_call0_prologue(code)
        }
    }
}

/// Registers an architecture, its calling conventions and (on the windowed
/// cores) the call0 function recognizer for every core.
pub fn register() {
    for core in Core::ALL {
        let arch = register_architecture(core.arch_name(), move |custom_handle, handle| {
            Xtensa::new(core, handle, custom_handle)
        });
        let conventions = cc::register(arch);
        if core.windowed() {
            arch.register_function_recognizer(cc::WindowRecognizer {
                call0: conventions.call0,
            });
        }
    }
}

/// Is `code` the start of a windowed-ABI function (`entry a1, n`)?
pub fn is_entry_insn(code: &[u8]) -> bool {
    code.len() >= 3 && code[0] == 0x36 && code[1] & 0x0f == 0x01
}

/// Does `code` start with `addi a1, a1, n`, the stack adjustment that
/// begins a call0-ABI function with a frame?
pub fn is_call0_prologue(code: &[u8]) -> bool {
    // addi at, as, imm8: byte0 = t << 4 | 2, byte1 = 0xc << 4 | s.
    code.starts_with(&[0x12, 0xc1])
}

/// Is `code` an `entry a1, n` with a plausible frame size (at most 4 KiB)?
/// Stricter than [`is_entry_insn`], for scanning code for function starts.
pub fn is_plausible_entry(code: &[u8]) -> bool {
    // entry as, imm12*8: byte0 = 0x36, byte1 = imm[3:0] << 4 | as,
    // byte2 = imm[11:4].
    is_entry_insn(code) && code[2] < 0x20
}

/// The offsets in `code`, loaded at `addr`, of the 4-byte aligned
/// plausible `entry` instructions.
pub fn entry_offsets(addr: u32, code: &[u8]) -> impl Iterator<Item = usize> {
    let skip = (4 - (addr % 4) as usize) % 4;
    (skip..(code.len() + 1).saturating_sub(3))
        .step_by(4)
        .filter(|&off| is_plausible_entry(&code[off..off + 3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_scan_bounds() {
        let entry = [0x36, 0x41, 0x00];
        assert_eq!(entry_offsets(0x4008_0000, &[0x36, 0x41]).count(), 0);
        assert_eq!(entry_offsets(0x4008_0000, &entry).collect::<Vec<_>>(), [0]);
        let mut code = vec![0u8; 8];
        code.extend(entry);
        assert_eq!(entry_offsets(0x4008_0000, &code).collect::<Vec<_>>(), [8]);
        // An unaligned segment start: only absolute 4-byte alignment counts.
        let mut code = vec![0u8; 3];
        code.extend(entry);
        assert_eq!(entry_offsets(0x4008_0001, &code).collect::<Vec<_>>(), [3]);
        assert_eq!(entry_offsets(0x4008_0000, &code).count(), 0);
    }

    #[test]
    fn function_starts_and_prologues() {
        let code = [0, 0, 0, 0, 0x36, 0x41, 0x00, 0x1d, 0xf0];
        assert_eq!(
            Core::Esp32.function_starts(0x4008_0000, &code),
            [0x4008_0004]
        );
        // call0 code is not scanned.
        assert!(Core::Esp8266.function_starts(0x4010_0000, &code).is_empty());

        let call0 = [0x12, 0xc1, 0xf0];
        assert!(Core::Esp8266.is_prologue(&call0));
        assert!(!Core::Esp8266.is_prologue(&code[4..]));
        assert!(Core::Esp32S3.is_prologue(&code[4..]));
        assert!(!Core::Esp32S3.is_prologue(&call0));
    }
}
