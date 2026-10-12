//! Re-encoding an instruction's address or immediate field, shared by the
//! patches (`crate::patch`) and the ELF relocations
//! (`crate::relocation`). Pure functions on instruction words and byte
//! slices.
//!
//! Every new encoding is checked by decoding it: an instruction is only
//! rewritten if it decodes to the same opcode and length, with the same
//! operands except for the one that was set. An encoding mistake makes the
//! rewrite fail rather than produce the wrong instruction.

use std::fmt;
use std::ops::RangeInclusive;

use xtensa_isa::{Instruction, Isa, Opcode, Operand};

/// Why a field cannot be set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The bytes are not an instruction.
    Undecodable,
    /// The instruction has no field this module knows how to set.
    Unsupported(Opcode),
    /// The target or value does not fit the field.
    OutOfRange,
    /// A call target or literal that is not 4-byte aligned.
    Misaligned,
    /// An `l32r` literal at or after the instruction: `l32r` only reaches
    /// backwards.
    LiteralAfterUse,
    /// The new encoding did not decode as expected (a bug).
    Unverified,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Undecodable => f.write_str("not a valid instruction"),
            Error::Unsupported(op) => write!(f, "{op:?} has no relocatable field"),
            Error::OutOfRange => f.write_str("target out of range"),
            Error::Misaligned => f.write_str("target not 4-byte aligned"),
            Error::LiteralAfterUse => f.write_str("literal placed after its l32r"),
            Error::Unverified => f.write_str("re-encoded instruction failed to verify"),
        }
    }
}

/// The range of `j`'s and the calls' 18-bit signed offsets (in bytes for
/// `j`, in words for the calls).
const OFFSET18: RangeInclusive<i32> = -(1 << 17)..=(1 << 17) - 1;
/// The range of `l32r`'s offset in bytes: 1 to 65536 words back.
const L32R: RangeInclusive<i32> = -(1 << 18)..=-4;
/// The range of the BRI12 branches' (`beqz`, `bnez`, `bltz`, `bgez`) offset.
const IMM12: RangeInclusive<i32> = -2048..=2047;
/// The range of the other wide branches' 8-bit signed offset.
const IMM8: RangeInclusive<i32> = -128..=127;
/// The range of the loops' 8-bit unsigned offset.
const UIMM8: RangeInclusive<i32> = 0..=255;
/// The range of `beqz.n`'s and `bnez.n`'s 6-bit unsigned offset.
const UIMM6: RangeInclusive<i32> = 0..=63;
/// The range of `movi.n`'s immediate.
const MOVI_N: RangeInclusive<i32> = -32..=95;
/// The range of `movi`'s 12-bit signed immediate.
pub const MOVI: RangeInclusive<i32> = -2048..=2047;

pub fn decode(isa: &Isa, data: &[u8], addr: u32) -> Option<Instruction> {
    isa.decode(data, addr).ok()
}

/// The instruction word `word`, `len` bytes long, if it decodes at `addr` to
/// an instruction satisfying `check`.
pub fn verified(
    isa: &Isa,
    word: u32,
    len: usize,
    addr: u32,
    check: impl FnOnce(&Instruction) -> bool,
) -> Option<Vec<u8>> {
    let bytes = word.to_le_bytes()[..len].to_vec();
    let insn = decode(isa, &bytes, addr)?;
    (insn.len() == len && check(&insn)).then_some(bytes)
}

/// Copies `bytes` over the start of `data`, if they fit.
pub fn write(data: &mut [u8], bytes: Option<Vec<u8>>) -> bool {
    match bytes {
        Some(bytes) if data.len() >= bytes.len() => {
            data[..bytes.len()].copy_from_slice(&bytes);
            true
        }
        _ => false,
    }
}

fn check(value: i32, range: RangeInclusive<i32>) -> Result<u32, Error> {
    if range.contains(&value) {
        Ok(value as u32)
    } else {
        Err(Error::OutOfRange)
    }
}

fn aligned(addr: u32) -> Result<(), Error> {
    if addr & 3 == 0 {
        Ok(())
    } else {
        Err(Error::Misaligned)
    }
}

/// The instruction word `raw` of `op`, at `pc`, with its address field
/// set to reach `value`, or (for `movi` and `movi.n`) its immediate set to
/// `value`. Fields are as in the Xtensa ISA reference:
///
/// * `j`: 18-bit signed offset from `pc + 4`, bits 6..24.
/// * `call0`..`call12`: 18-bit signed word offset from `(pc & ~3) + 4`,
///   bits 6..24; the target must be 4-byte aligned.
/// * `l32r`: 16-bit word offset, ones-extended (so always negative), from
///   `(pc + 3) & ~3`, bits 8..24; the literal must be 4-byte aligned.
/// * BRI12 branches: 12-bit signed offset from `pc + 4`, bits 12..24.
/// * BRI8 and RRI8 branches: 8-bit signed offset from `pc + 4`, bits 16..24.
/// * `loop`, `loopnez`, `loopgtz`: 8-bit unsigned offset of the loop end
///   from `pc + 4`, bits 16..24.
/// * `beqz.n`, `bnez.n`: 6-bit unsigned offset from `pc + 4`, bits 4..6
///   (high two) and 12..16 (low four).
/// * `movi`: 12-bit signed immediate, bits 8..12 (high four) and 16..24.
/// * `movi.n`: 7-bit immediate (-32..=95), bits 4..7 (high three) and
///   12..16 (low four).
pub fn set_field(op: Opcode, raw: u32, pc: u32, value: u32) -> Result<u32, Error> {
    use Opcode as O;
    let rel = value.wrapping_sub(pc.wrapping_add(4)) as i32;
    Ok(match op {
        O::J => raw & 0x3f | (check(rel, OFFSET18)? & 0x3_ffff) << 6,
        O::Call0 | O::Call4 | O::Call8 | O::Call12 => {
            aligned(value)?;
            let base = (pc & !3).wrapping_add(4);
            let words = value.wrapping_sub(base) as i32 >> 2;
            raw & 0x3f | (check(words, OFFSET18)? & 0x3_ffff) << 6
        }
        O::L32r => {
            aligned(value)?;
            let base = pc.wrapping_add(3) & !3;
            let offset = value.wrapping_sub(base) as i32;
            if offset >= 0 {
                return Err(Error::LiteralAfterUse);
            }
            raw & 0xff | (check(offset, L32R)? >> 2 & 0xffff) << 8
        }
        O::Beqz | O::Bnez | O::Bltz | O::Bgez => raw & 0xfff | (check(rel, IMM12)? & 0xfff) << 12,
        O::Beq
        | O::Bne
        | O::Blt
        | O::Bge
        | O::Bltu
        | O::Bgeu
        | O::Bany
        | O::Bnone
        | O::Ball
        | O::Bnall
        | O::Bbc
        | O::Bbs
        | O::Bbci
        | O::Bbsi
        | O::Beqi
        | O::Bnei
        | O::Blti
        | O::Bgei
        | O::Bltui
        | O::Bgeui
        | O::Bf
        | O::Bt => raw & 0xffff | (check(rel, IMM8)? & 0xff) << 16,
        O::Loop | O::Loopnez | O::Loopgtz => raw & 0xffff | check(rel, UIMM8)? << 16,
        O::BeqzN | O::BnezN => {
            let off = check(rel, UIMM6)?;
            raw & 0xffff_0fcf | (off >> 4) << 4 | (off & 0xf) << 12
        }
        O::Movi => {
            let imm = check(value as i32, MOVI)? & 0xfff;
            raw & 0xff00_f0ff | (imm >> 8) << 8 | (imm & 0xff) << 16
        }
        O::MoviN => {
            let imm = check(value as i32, MOVI_N)? & 0x7f;
            raw & 0xffff_0f8f | (imm >> 4) << 4 | (imm & 0xf) << 12
        }
        op => return Err(Error::Unsupported(op)),
    })
}

/// The index of the operand [`set_field`] sets: the code target or
/// literal address, or the immediate of `movi`/`movi.n`.
fn field_operand(insn: &Instruction) -> Option<usize> {
    let ops = insn.operands();
    match insn.opcode() {
        Opcode::Movi | Opcode::MoviN => ops.iter().rposition(|o| matches!(o, Operand::Imm(_))),
        _ => ops
            .iter()
            .rposition(|o| matches!(o, Operand::Target(_) | Operand::Literal(_))),
    }
}

/// `insn`'s operands with the one [`set_field`] sets replaced by `value`.
fn expected_operands(insn: &Instruction, value: u32) -> Option<Vec<Operand>> {
    let index = field_operand(insn)?;
    let mut ops = insn.operands().to_vec();
    ops[index] = match ops[index] {
        Operand::Target(_) => Operand::Target(value),
        Operand::Literal(_) => Operand::Literal(value),
        Operand::Imm(_) => Operand::Imm(value as i32),
        _ => return None,
    };
    Some(ops)
}

/// The instruction at the start of `data`, at `pc`, re-encoded so that its
/// address field reaches `value` (or its immediate is `value`), checked by
/// decoding.
pub fn retarget(isa: &Isa, data: &[u8], pc: u32, value: u32) -> Result<Vec<u8>, Error> {
    let insn = decode(isa, data, pc).ok_or(Error::Undecodable)?;
    let word = set_field(insn.opcode(), insn.raw(), pc, value)?;
    let expected = expected_operands(&insn, value).ok_or(Error::Unsupported(insn.opcode()))?;
    verified(isa, word, insn.len(), pc, |new| {
        new.opcode() == insn.opcode() && new.operands() == expected.as_slice()
    })
    .ok_or(Error::Unverified)
}

/// A `j` from `pc` to `target`.
pub fn jump(isa: &Isa, pc: u32, target: u32) -> Option<Vec<u8>> {
    // j (CALL format: op0 = 6, n = 0).
    let word = set_field(Opcode::J, 0x6, pc, target).ok()?;
    verified(isa, word, 3, pc, |new| {
        new.opcode() == Opcode::J && new.operands() == [Operand::Target(target)]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exemplars::EXEMPLARS;
    use xtensa_isa::{ControlFlow, Features};

    const ISAS: [Isa; 4] = [Isa::ESP8266, Isa::ESP32, Isa::ESP32S2, Isa::ESP32S3];

    fn bytes(word: u32, len: usize) -> Vec<u8> {
        word.to_le_bytes()[..len].to_vec()
    }

    /// The value of the operand `retarget` sets.
    fn field_value(insn: &Instruction) -> Option<u32> {
        match insn.operands()[field_operand(insn)?] {
            Operand::Target(t) | Operand::Literal(t) => Some(t),
            Operand::Imm(i) => Some(i as u32),
            _ => None,
        }
    }

    /// Retargets the instruction `word` at `pc` to every value in `values`
    /// and checks each result by decoding: either it decodes with the new
    /// value and nothing else changed, or `retarget` failed and says why.
    fn round_trip(isa: &Isa, word: u32, len: usize, pc: u32, values: &[u32]) -> Option<Opcode> {
        let data = bytes(word, len);
        let insn = decode(isa, &data, pc)?;
        if insn.len() != len {
            return None;
        }
        let Some(original) = field_value(&insn) else {
            assert!(
                matches!(retarget(isa, &data, pc, 0), Err(Error::Unsupported(_))),
                "{insn}"
            );
            return None;
        };
        // Setting the field to its current value changes nothing.
        assert_eq!(
            retarget(isa, &data, pc, original).as_deref(),
            Ok(&data[..]),
            "{insn}"
        );
        for &value in values {
            match retarget(isa, &data, pc, value) {
                Ok(new) => {
                    let new = decode(isa, &new, pc).unwrap();
                    assert_eq!(new.opcode(), insn.opcode());
                    assert_eq!(field_value(&new), Some(value), "{insn} -> {value:#x}");
                    match (insn.control_flow(), new.control_flow()) {
                        (ControlFlow::Branch { .. }, ControlFlow::Branch { target })
                        | (ControlFlow::Jump { .. }, ControlFlow::Jump { target })
                        | (ControlFlow::Call { .. }, ControlFlow::Call { target, .. })
                        | (ControlFlow::Loop { .. }, ControlFlow::Loop { end: target, .. }) => {
                            assert_eq!(target, value, "{insn}")
                        }
                        (a, b) => assert_eq!(a, b, "{insn}"),
                    }
                }
                Err(Error::OutOfRange | Error::Misaligned | Error::LiteralAfterUse) => {}
                Err(e) => panic!("{insn} -> {value:#x}: {e}"),
            }
        }
        Some(insn.opcode())
    }

    /// Targets around `pc`, in and out of every field's range.
    fn targets(pc: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for d in [
            0i64, 1, 2, 3, 4, 5, 63, 64, 67, 68, 127, 128, 131, 132, 255, 259, 260, 2047, 2051,
            2052, 0x1_ffff, 0x2_0003, 0x2_0004, 0x7_ffff, 0x8_0000, 0x8_0003, 0x8_0004,
        ] {
            for d in [d, -d, -d - 1, -d - 4] {
                out.push((pc as i64 + d) as u32);
            }
        }
        out.extend([
            0,
            0xffff_fffc,
            0x3f,
            0x5f,
            0x60,
            0xffff_ffe0,
            0xffff_ffdf,
            0x7ff,
            0x800,
        ]);
        out.extend([0xffff_f800, 0xffff_f7ff, 0x4000_0000, (pc + 3) & !3]);
        out.extend((1..=8).map(|w| ((pc + 3) & !3).wrapping_sub(w * 4)));
        out.extend([((pc + 3) & !3).wrapping_sub(0x4_0000)]);
        out.extend([((pc + 3) & !3).wrapping_sub(0x4_0004)]);
        out
    }

    #[test]
    fn every_relocatable_form() {
        // Every word of the formats with an address field: op0 = 1 (l32r),
        // 5 (calls), 6 (j, BRI12/BRI8 branches, loops), 7 (RRI8 branches),
        // 2 with r = 0xa (movi) and 0xc (narrow branches, movi.n).
        for isa in ISAS {
            let mut seen = std::collections::HashSet::new();
            for pc in [0x4008_1002u32, 0x4000_0000, 0x3, 0x1_0021] {
                let values = targets(pc);
                for (op0, len, step) in [
                    (0x1, 3, 0x1_0001),
                    (0x5, 3, 0x101),
                    (0x6, 3, 0x0fd),
                    (0x7, 3, 0x0fd),
                    (0x2, 3, 0x0fd),
                    (0xc, 2, 1),
                ] {
                    let count = 1u32 << (len * 8 - 4);
                    for high in (0..count).step_by(step) {
                        let mut word = high << 4 | op0;
                        if op0 == 0x2 {
                            word = word & !0xf000 | 0xa000;
                        }
                        if let Some(op) = round_trip(&isa, word, len, pc, &values) {
                            seen.insert(op);
                        }
                    }
                }
                // Explicit forms the strided sweep may miss.
                for (word, len) in [
                    (0x00_0005u32, 3),
                    (0x00_0015, 3),
                    (0x00_0025, 3),
                    (0x00_0035, 3),
                    (0x00_0006, 3),
                    (0xff_ff21, 3),
                    (0x00_8876, 3),
                    (0x00_9876, 3),
                    (0x00_a876, 3),
                    (0x10_0076, 3),
                    (0x10_1076, 3),
                    (0x00_a022, 3),
                    (0x052c, 2),
                    (0x8c, 2),
                    (0xcc, 2),
                ] {
                    if let Some(op) = round_trip(&isa, word, len, pc, &values) {
                        seen.insert(op);
                    }
                }
            }
            let mut expect = vec![
                Opcode::J,
                Opcode::Call0,
                Opcode::L32r,
                Opcode::Beqz,
                Opcode::Bnez,
                Opcode::Bltz,
                Opcode::Bgez,
                Opcode::Beq,
                Opcode::Bne,
                Opcode::Blt,
                Opcode::Bge,
                Opcode::Bltu,
                Opcode::Bgeu,
                Opcode::Bany,
                Opcode::Bnone,
                Opcode::Ball,
                Opcode::Bnall,
                Opcode::Bbc,
                Opcode::Bbs,
                Opcode::Bbci,
                Opcode::Bbsi,
                Opcode::Beqi,
                Opcode::Bnei,
                Opcode::Blti,
                Opcode::Bgei,
                Opcode::Bltui,
                Opcode::Bgeui,
                Opcode::BeqzN,
                Opcode::BnezN,
                Opcode::Movi,
                Opcode::MoviN,
            ];
            if isa.has(Features::WINDOWED) {
                expect.extend([Opcode::Call4, Opcode::Call8, Opcode::Call12]);
            }
            if isa.has(Features::BOOLEAN) {
                expect.extend([Opcode::Bf, Opcode::Bt]);
            }
            if isa.has(Features::LOOP) {
                expect.extend([Opcode::Loop, Opcode::Loopnez, Opcode::Loopgtz]);
            }
            for op in expect {
                assert!(seen.contains(&op), "{op:?} not covered on {isa:?}");
            }
        }
    }

    #[test]
    fn exemplars() {
        for isa in ISAS {
            for &(word, len) in EXEMPLARS {
                let pc = 0x4008_1002;
                round_trip(&isa, word, len as usize, pc, &targets(pc));
            }
        }
    }

    #[test]
    fn known_encodings() {
        let isa = Isa::ESP32;
        // call8 at 0x5 (as in an object's .text): target = (pc & ~3) + 4 +
        // offset * 4.
        assert_eq!(set_field(Opcode::Call8, 0x25, 0x5, 0x24), Ok(0x25 | 7 << 6));
        assert_eq!(
            set_field(Opcode::Call8, 0x25, 0x5, 0x26),
            Err(Error::Misaligned)
        );
        // l32r at 0x81 reading 0x0: base (0x81 + 3) & ~3 = 0x84, 33 words back.
        assert_eq!(
            set_field(Opcode::L32r, 0x21, 0x81, 0x0),
            Ok(0x21 | (0x1_0000 - 33) << 8)
        );
        assert_eq!(
            set_field(Opcode::L32r, 0x21, 0x81, 0x84),
            Err(Error::LiteralAfterUse)
        );
        assert_eq!(
            set_field(Opcode::L32r, 0x21, 0x81, 0x2),
            Err(Error::Misaligned)
        );
        assert_eq!(
            set_field(Opcode::L32r, 0x21, 0x3_fffd, 0x0),
            Ok(0x21),
            "the furthest literal"
        );
        assert_eq!(
            set_field(Opcode::L32r, 0x21, 0x4_0001, 0x0),
            Err(Error::OutOfRange)
        );
        // j at an unaligned pc: target = pc + 4 + offset.
        assert_eq!(jump(&isa, 0x1001, 0x1001), Some(vec![0x06, 0xff, 0xff]));
        assert_eq!(jump(&isa, 0, 0x2_0003), Some(vec![0xc6, 0xff, 0x7f]));
        assert_eq!(jump(&isa, 0, 0x2_0004), None);
        // beqz.n a4, . + 4 + 0x10 (from an object: d48c at 0x67 -> 0x78).
        let data = [0x8c, 0xd4];
        assert_eq!(retarget(&isa, &data, 0x67, 0x78), Ok(data.to_vec()));
        assert_eq!(retarget(&isa, &data, 0x67, 0x6b), Ok(vec![0x8c, 0x04]));
        assert_eq!(retarget(&isa, &data, 0x67, 0x6a), Err(Error::OutOfRange));
        assert_eq!(
            retarget(&isa, &data, 0x67, 0x6b + 64),
            Err(Error::OutOfRange)
        );
        // loop a8, end (058876 at 0x51 -> 0x5a): unsigned offset.
        let data = [0x76, 0x88, 0x05];
        assert_eq!(retarget(&isa, &data, 0x51, 0x5a), Ok(data.to_vec()));
        assert_eq!(
            retarget(&isa, &data, 0x51, 0x55),
            Ok(vec![0x76, 0x88, 0x00])
        );
        assert_eq!(
            retarget(&isa, &data, 0x51, 0x55 + 255),
            Ok(vec![0x76, 0x88, 0xff])
        );
        assert_eq!(retarget(&isa, &data, 0x51, 0x54), Err(Error::OutOfRange));
        assert_eq!(
            retarget(&isa, &data, 0x51, 0x55 + 256),
            Err(Error::OutOfRange)
        );
        // movi a2, 0x7ff / -0x800
        let data = [0x22, 0xa0, 0x00];
        assert_eq!(retarget(&isa, &data, 0, 0x7ff), Ok(vec![0x22, 0xa7, 0xff]));
        assert_eq!(
            retarget(&isa, &data, 0, -0x800i32 as u32),
            Ok(vec![0x22, 0xa8, 0x00])
        );
        assert_eq!(retarget(&isa, &data, 0, 0x800), Err(Error::OutOfRange));
        // Not relocatable.
        assert_eq!(
            retarget(&isa, &[0xf0, 0x20, 0x00], 0, 0),
            Err(Error::Unsupported(Opcode::Nop))
        );
        assert_eq!(retarget(&isa, &[], 0, 0), Err(Error::Undecodable));
    }
}
