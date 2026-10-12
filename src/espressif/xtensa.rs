//! What the firmware loader looks for in the code of Espressif's Xtensa
//! cores: windowed functions' `entry` instructions and call0 prologues,
//! decoded with the `xtensa` decoder. Their code is analyzed with the
//! Xtensa architectures, looked up by name (see [`XtensaCore::arch_name`]).

use xtensa_isa::{Features, Isa, Opcode, Operand};

/// An Xtensa core configuration of Espressif's chips. Each has an
/// architecture of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XtensaCore {
    Esp8266,
    Esp32,
    Esp32S2,
    Esp32S3,
}

impl XtensaCore {
    pub fn isa(self) -> Isa {
        match self {
            XtensaCore::Esp8266 => Isa::ESP8266,
            XtensaCore::Esp32 => Isa::ESP32,
            XtensaCore::Esp32S2 => Isa::ESP32S2,
            XtensaCore::Esp32S3 => Isa::ESP32S3,
        }
    }

    /// The name of the core's architecture (and standalone platform), as
    /// the Xtensa architecture plugin registers it.
    pub fn arch_name(self) -> &'static str {
        match self {
            XtensaCore::Esp8266 => "xtensa-esp8266",
            XtensaCore::Esp32 => "xtensa-esp32",
            XtensaCore::Esp32S2 => "xtensa-esp32s2",
            XtensaCore::Esp32S3 => "xtensa-esp32s3",
        }
    }

    /// Does the core have register windows, so that its code uses the
    /// windowed ABI?
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
    /// `entry` on the windowed cores, `addi a1, a1, n` on the others?
    pub fn is_prologue(self, code: &[u8]) -> bool {
        if self.windowed() {
            is_entry_insn(code)
        } else {
            is_call0_prologue(code)
        }
    }
}

/// The stack pointer register.
const SP: u8 = 1;

/// The frame size of the `entry a1, n` at the start of `code`, if it starts
/// with one.
fn entry_frame(code: &[u8]) -> Option<i32> {
    // Every core with register windows encodes `entry` the same way.
    let insn = Isa::ESP32.decode(code.get(..3)?, 0).ok()?;
    match (insn.opcode(), insn.operands()) {
        (Opcode::Entry, &[Operand::Ar(SP), Operand::Imm(frame)]) => Some(frame),
        _ => None,
    }
}

/// Is `code` the start of a windowed-ABI function (`entry a1, n`)?
pub fn is_entry_insn(code: &[u8]) -> bool {
    entry_frame(code).is_some()
}

/// Does `code` start with `addi a1, a1, n`, the stack adjustment that
/// begins a call0-ABI function with a frame?
pub fn is_call0_prologue(code: &[u8]) -> bool {
    let Some(insn) = code
        .get(..3)
        .and_then(|code| Isa::ESP8266.decode(code, 0).ok())
    else {
        return false;
    };
    matches!(
        (insn.opcode(), insn.operands()),
        (
            Opcode::Addi,
            &[Operand::Ar(SP), Operand::Ar(SP), Operand::Imm(_)]
        )
    )
}

/// Is `code` an `entry a1, n` with a plausible frame size (under 4 KiB)?
/// Stricter than [`is_entry_insn`], for scanning code for function starts.
fn is_plausible_entry(code: &[u8]) -> bool {
    entry_frame(code).is_some_and(|frame| frame < 0x1000)
}

/// The offsets in `code`, loaded at `addr`, of the 4-byte aligned
/// plausible `entry` instructions.
fn entry_offsets(addr: u32, code: &[u8]) -> impl Iterator<Item = usize> {
    // `entry`'s first byte, checked before decoding: most words are not one.
    const ENTRY: u8 = 0x36;
    let skip = (4 - (addr % 4) as usize) % 4;
    (skip..(code.len() + 1).saturating_sub(3))
        .step_by(4)
        .filter(|&off| code[off] == ENTRY && is_plausible_entry(&code[off..off + 3]))
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
            XtensaCore::Esp32.function_starts(0x4008_0000, &code),
            [0x4008_0004]
        );
        // call0 code is not scanned.
        assert!(
            XtensaCore::Esp8266
                .function_starts(0x4010_0000, &code)
                .is_empty()
        );

        let call0 = [0x12, 0xc1, 0xf0];
        assert!(XtensaCore::Esp8266.is_prologue(&call0));
        assert!(!XtensaCore::Esp8266.is_prologue(&code[4..]));
        assert!(XtensaCore::Esp32S3.is_prologue(&code[4..]));
        assert!(!XtensaCore::Esp32S3.is_prologue(&call0));
    }

    /// The decoder agrees with the encodings: `entry as, imm12 * 8` is
    /// `0x36, imm12[3:0] << 4 | as, imm12[11:4]`, and `addi at, as, imm8`
    /// is `t << 4 | 2, 0xc << 4 | s, imm8`.
    #[test]
    fn prologue_encodings() {
        for byte1 in 0..=0xffu8 {
            for byte2 in [0x00, 0x01, 0x1f, 0x20, 0xff] {
                let code = [0x36, byte1, byte2];
                assert_eq!(is_entry_insn(&code), byte1 & 0x0f == 1, "{code:x?}");
                assert_eq!(
                    is_plausible_entry(&code),
                    byte1 & 0x0f == 1 && byte2 < 0x20,
                    "{code:x?}"
                );
                let code = [byte1, byte2, 0x80];
                assert_eq!(
                    is_call0_prologue(&code),
                    byte1 == 0x12 && byte2 == 0xc1,
                    "{code:x?}"
                );
            }
        }
        // Too short to hold either.
        assert!(!is_entry_insn(&[0x36, 0x41]));
        assert!(!is_call0_prologue(&[0x12, 0xc1]));
    }
}
