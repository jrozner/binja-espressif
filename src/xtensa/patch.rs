//! Instruction patches for Binary Ninja's patching actions: nop out,
//! invert a branch, make a branch unconditional and replace a call with a
//! return value. Pure functions on byte slices; the architecture's patching
//! callbacks wrap them.
//!
//! Every patch is checked by decoding the patched bytes before they are
//! written, so an encoding mistake makes the patch fail rather than produce
//! the wrong instruction (see `crate::xtensa::encode`).

use xtensa_isa::{ControlFlow, Features, Instruction, Isa, Opcode, Operand};

use crate::xtensa::encode::{self, MOVI, verified, write};

/// `nop` (RRR: op0 = 0, op1 = 0, op2 = 0, r = 2, s = 0, t = 0xf).
pub const NOP: [u8; 3] = [0xf0, 0x20, 0x00];
/// `nop.n` (RRRN: op0 = 0xd, r = 0xf, s = 0, t = 3).
pub const NOP_N: [u8; 2] = [0x3d, 0xf0];

/// The condition with the opposite sense, and the bit of the instruction
/// word that selects between the two.
fn inverse(op: Opcode) -> Option<(Opcode, u32)> {
    use Opcode as O;
    // RRI8 (op0 = 7): the condition is r (bits 12..15); r ^ 8 inverts.
    const R8: u32 = 8 << 12;
    // BRI12/BRI8 (op0 = 6) and the narrow branches (op0 = 0xc): bit 6
    // (m ^ 1, or t ^ 4 for beqz.n/bnez.n) inverts.
    const BIT6: u32 = 1 << 6;
    // bf/bt: r = 0/1.
    const R1: u32 = 1 << 12;
    let pairs = [
        (O::Beq, O::Bne, R8),
        (O::Blt, O::Bge, R8),
        (O::Bltu, O::Bgeu, R8),
        (O::Bnone, O::Bany, R8),
        (O::Ball, O::Bnall, R8),
        (O::Bbc, O::Bbs, R8),
        (O::Bbci, O::Bbsi, R8),
        (O::Beqz, O::Bnez, BIT6),
        (O::Bltz, O::Bgez, BIT6),
        (O::Beqi, O::Bnei, BIT6),
        (O::Blti, O::Bgei, BIT6),
        (O::Bltui, O::Bgeui, BIT6),
        (O::Bf, O::Bt, R1),
        (O::BeqzN, O::BnezN, BIT6),
    ];
    pairs.iter().find_map(|&(a, b, bit)| {
        if op == a {
            Some((b, bit))
        } else if op == b {
            Some((a, bit))
        } else {
            None
        }
    })
}

fn decode(isa: &Isa, data: &[u8], addr: u64) -> Option<Instruction> {
    encode::decode(isa, data, addr as u32)
}

/// The conditional branch at the start of `data` and its target. Loop
/// instructions (`loopnez`, `loopgtz`) are not branches here.
fn branch(isa: &Isa, data: &[u8], addr: u64) -> Option<(Instruction, u32)> {
    let insn = decode(isa, data, addr)?;
    match insn.control_flow() {
        ControlFlow::Branch { target } if inverse(insn.opcode()).is_some() => Some((insn, target)),
        _ => None,
    }
}

/// Is the instruction at `addr` a conditional branch that
/// [`invert_branch`] can invert (and so one that can be nopped out)?
pub fn is_conditional_branch(isa: &Isa, data: &[u8], addr: u64) -> bool {
    branch(isa, data, addr).is_some()
}

/// Can [`always_branch`] make the instruction at `addr` unconditional? Only
/// the 3-byte branches have room for a `j`.
pub fn can_always_branch(isa: &Isa, data: &[u8], addr: u64) -> bool {
    branch(isa, data, addr).is_some_and(|(insn, _)| insn.len() == 3)
}

/// Inverts the condition of the branch at `addr`, keeping its target.
pub fn invert_branch(isa: &Isa, data: &mut [u8], addr: u64) -> bool {
    let Some((insn, target)) = branch(isa, data, addr) else {
        return false;
    };
    let Some((inverse, bit)) = inverse(insn.opcode()) else {
        return false;
    };
    let patched = verified(isa, insn.raw() ^ bit, insn.len(), addr as u32, |new| {
        new.opcode() == inverse
            && new.control_flow() == (ControlFlow::Branch { target })
            && operands_kept(&insn, new)
    });
    write(data, patched)
}

/// The operands other than the target are unchanged (`bbci`/`bbsi` keep
/// their bit number, the immediate branches their constant).
fn operands_kept(old: &Instruction, new: &Instruction) -> bool {
    let tail = |insn: &Instruction| -> Vec<Operand> {
        let ops = insn.operands();
        ops[..ops.len() - 1].to_vec()
    };
    tail(old) == tail(new)
}

/// Replaces the 3-byte conditional branch at `addr` with a `j` to its
/// target. A narrow branch has no room for a `j`, so is left alone.
pub fn always_branch(isa: &Isa, data: &mut [u8], addr: u64) -> bool {
    let Some((insn, target)) = branch(isa, data, addr) else {
        return false;
    };
    if insn.len() != 3 {
        return false;
    }
    write(data, encode::jump(isa, addr as u32, target))
}

/// Fills `data` with `nop` and `nop.n`. Any length from 2 bytes works with
/// the code density option; without it, only multiples of 3.
pub fn convert_to_nop(isa: &Isa, data: &mut [u8]) -> bool {
    let len = data.len();
    let narrow = match len % 3 {
        _ if len < 2 => return false,
        0 => 0,
        _ if !isa.has(Features::DENSITY) => return false,
        2 => 1,
        _ if len >= 4 => 2,
        _ => return false,
    };
    let (wide, rest) = data.split_at_mut(len - 2 * narrow);
    for chunk in wide.as_chunks_mut::<3>().0 {
        *chunk = NOP;
    }
    for chunk in rest.as_chunks_mut::<2>().0 {
        *chunk = NOP_N;
    }
    true
}

/// The register a call at `addr` returns its value in, as the caller sees
/// it: `a2` for `call0`/`callx0`, `a(2 + n)` for the windowed `callN`/`callxN`.
fn return_register(isa: &Isa, data: &[u8], addr: u64) -> Option<(Instruction, u32)> {
    let insn = decode(isa, data, addr)?;
    let window = match insn.control_flow() {
        ControlFlow::Call { window, .. } | ControlFlow::IndirectCall { window } => window,
        _ => return None,
    };
    Some((insn, 2 + u32::from(window)))
}

/// Can [`skip_and_return_value`] replace the instruction at `addr`?
pub fn is_call(isa: &Isa, data: &[u8], addr: u64) -> bool {
    return_register(isa, data, addr).is_some()
}

/// Replaces the call at `addr` with a `movi` of `value` to the register the
/// call returns in. Fails for values outside `movi`'s -2048..=2047.
pub fn skip_and_return_value(isa: &Isa, data: &mut [u8], addr: u64, value: u64) -> bool {
    let Some((insn, reg)) = return_register(isa, data, addr) else {
        return false;
    };
    // The value as a 32-bit register sees it: sign-extended or not.
    let Ok(value) = i32::try_from(value as i64).or(u32::try_from(value).map(|v| v as i32)) else {
        return false;
    };
    if insn.len() != 3 || !MOVI.contains(&value) {
        return false;
    }
    // movi at, imm12 (RRI8: op0 = 2, r = 0xa, s = imm12[11:8], imm8 = imm12[7:0]).
    let Ok(word) = encode::set_field(Opcode::Movi, 0xa << 12 | reg << 4 | 0x2, 0, value as u32)
    else {
        return false;
    };
    let patched = verified(isa, word, 3, addr as u32, |new| {
        new.opcode() == Opcode::Movi
            && new.operands() == [Operand::Ar(reg as u8), Operand::Imm(value)]
    });
    write(data, patched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xtensa::exemplars::EXEMPLARS;

    const ISAS: [Isa; 4] = [Isa::ESP8266, Isa::ESP32, Isa::ESP32S2, Isa::ESP32S3];
    const ADDR: u64 = 0x4008_1002;

    fn bytes(word: u32, len: usize) -> Vec<u8> {
        word.to_le_bytes()[..len].to_vec()
    }

    #[test]
    fn nops_decode() {
        for isa in ISAS {
            assert!(isa.has(Features::DENSITY));
            let nop = isa.decode(&NOP, 0).unwrap();
            assert_eq!((nop.opcode(), nop.len()), (Opcode::Nop, 3));
            let nop_n = isa.decode(&NOP_N, 0).unwrap();
            assert_eq!((nop_n.opcode(), nop_n.len()), (Opcode::NopN, 2));
        }
    }

    #[test]
    fn nop_fill() {
        for isa in ISAS {
            assert!(!convert_to_nop(&isa, &mut []));
            assert!(!convert_to_nop(&isa, &mut [0x55]));
            for len in 2..=13 {
                let mut data = vec![0x55; len];
                assert!(convert_to_nop(&isa, &mut data), "length {len}");
                let mut covered = 0;
                for insn in isa.disassemble(&data, 0) {
                    let insn = insn.1.unwrap();
                    assert!(matches!(insn.opcode(), Opcode::Nop | Opcode::NopN));
                    covered += insn.len();
                }
                assert_eq!(covered, len);
            }
        }
    }

    /// Checks every patch of the instruction `word`, `len` bytes long, at
    /// `addr`: a conditional branch inverts and nops out and (if wide)
    /// becomes a `j`; anything else is left alone.
    fn check(isa: &Isa, word: u32, len: usize, addr: u64) -> Option<Opcode> {
        let data = bytes(word, len);
        let Ok(insn) = isa.decode(&data, addr as u32) else {
            return None;
        };
        let target = match insn.control_flow() {
            ControlFlow::Branch { target } => Some(target),
            _ => None,
        };
        let mut patched = data.clone();
        if let Some(target) = target {
            assert!(is_conditional_branch(isa, &data, addr), "{insn}");
            assert!(invert_branch(isa, &mut patched, addr), "{insn}");
            let new = isa.decode(&patched, addr as u32).unwrap();
            assert_eq!(Some(new.opcode()), inverse(insn.opcode()).map(|i| i.0));
            assert_eq!(new.control_flow(), ControlFlow::Branch { target });
            assert_eq!(new.len(), insn.len());
            // Inverting twice restores the original.
            assert!(invert_branch(isa, &mut patched, addr));
            assert_eq!(patched, data);

            let mut patched = data.clone();
            assert_eq!(can_always_branch(isa, &data, addr), len == 3, "{insn}");
            assert_eq!(always_branch(isa, &mut patched, addr), len == 3, "{insn}");
            if len == 3 {
                let new = isa.decode(&patched, addr as u32).unwrap();
                assert_eq!(new.opcode(), Opcode::J);
                assert_eq!(new.control_flow(), ControlFlow::Jump { target });
            } else {
                assert_eq!(patched, data);
            }
        } else {
            assert!(!is_conditional_branch(isa, &data, addr), "{insn}");
            assert!(!can_always_branch(isa, &data, addr), "{insn}");
            assert!(!invert_branch(isa, &mut patched, addr), "{insn}");
            assert!(!always_branch(isa, &mut patched, addr), "{insn}");
            assert_eq!(patched, data);
        }
        Some(insn.opcode())
    }

    #[test]
    fn every_branch_encoding() {
        // Every word with a branch's op0: op0 = 6 (BRI12/BRI8, j, entry,
        // loops), 7 (RRI8 branches) and 0xc (narrow).
        for isa in ISAS {
            let mut seen = std::collections::HashSet::new();
            for (op0, len) in [(0x6, 3), (0x7, 3), (0xc, 2)] {
                let count = 1u32 << (len * 8 - 4);
                for high in (0..count).step_by(if len == 3 { 7 } else { 1 }) {
                    let word = high << 4 | op0;
                    for addr in [ADDR, 0x4000_0000, 0x3] {
                        if let Some(op) = check(&isa, word, len, addr) {
                            seen.insert(op);
                        }
                    }
                }
            }
            let mut expect = vec![
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
                Opcode::Beqz,
                Opcode::Bnez,
                Opcode::Bltz,
                Opcode::Bgez,
                Opcode::Beqi,
                Opcode::Bnei,
                Opcode::Blti,
                Opcode::Bgei,
                Opcode::Bltui,
                Opcode::Bgeui,
                Opcode::BeqzN,
                Opcode::BnezN,
                Opcode::J,
            ];
            if isa.has(Features::BOOLEAN) {
                expect.extend([Opcode::Bf, Opcode::Bt]);
            }
            if isa.has(Features::LOOP) {
                expect.extend([Opcode::Loop, Opcode::Loopnez, Opcode::Loopgtz]);
            }
            if isa.has(Features::WINDOWED) {
                expect.push(Opcode::Entry);
            }
            for op in expect {
                assert!(seen.contains(&op), "{op:?} not covered");
            }
        }
    }

    #[test]
    fn exemplars() {
        // Every exemplar is either a branch handled by every patch (narrow
        // ones only by invert and nop) or rejected by all of them.
        for isa in ISAS {
            for &(word, len) in EXEMPLARS {
                check(&isa, word, len as usize, ADDR);
            }
        }
    }

    #[test]
    fn known_encodings() {
        let isa = Isa::ESP32;
        // beq a2, a3, . + 4 + 0x10 <-> bne
        let mut data = [0x37, 0x12, 0x10];
        assert!(invert_branch(&isa, &mut data, 0));
        assert_eq!(data, [0x37, 0x92, 0x10]);
        // beqz.n a2, . + 4 + 5 <-> bnez.n: t bit 2 (byte 0, bit 6).
        let mut data = [0x8c, 0x52];
        assert_eq!(isa.decode(&data, 0).unwrap().opcode(), Opcode::BeqzN);
        assert!(invert_branch(&isa, &mut data, 0));
        assert_eq!(data, [0xcc, 0x52]);
        assert_eq!(isa.decode(&data, 0).unwrap().opcode(), Opcode::BnezN);
        // beqz a2, . + 4 - 4 -> j . + 4 - 4 at an unaligned pc: the target
        // is pc + 4 + offset, without aligning pc.
        let mut data = [0x16, 0xc2, 0xff];
        assert!(always_branch(&isa, &mut data, 0x1001));
        assert_eq!(data, [0x06, 0xff, 0xff]);
        assert_eq!(
            isa.decode(&data, 0x1001).unwrap().control_flow(),
            ControlFlow::Jump { target: 0x1001 }
        );
        // Neither entry nor a loop is a branch to patch.
        for code in [[0x36, 0x41, 0x00], [0x76, 0x82, 0x10], [0x76, 0x92, 0x10]] {
            assert!(!is_conditional_branch(&isa, &code, 0));
            assert!(!invert_branch(&isa, &mut code.clone(), 0));
        }
    }

    #[test]
    fn skip_and_return() {
        let calls = [
            (Opcode::Call0, 0x05, 2),
            (Opcode::Call4, 0x15, 6),
            (Opcode::Call8, 0x25, 10),
            (Opcode::Call12, 0x35, 14),
        ];
        for isa in ISAS {
            for (op, byte0, reg) in calls {
                // callN and callxN (callx: op0 = 0, r = 0, s = a3, t = 0xc + n/4).
                let callx = [0xc0 + (reg - 2) / 4 * 0x10, 0x03, 0x00];
                for code in [[byte0, 0x10, 0x00], callx] {
                    let Ok(insn) = isa.decode(&code, ADDR as u32) else {
                        assert!(!isa.has(Features::WINDOWED) && op != Opcode::Call0);
                        continue;
                    };
                    assert!(is_call(&isa, &code, ADDR), "{insn}");
                    for value in [
                        0u64,
                        1,
                        0x7ff,
                        (-1i64) as u64,
                        0xffff_ffff,
                        (-2048i64) as u64,
                    ] {
                        let mut data = code;
                        assert!(
                            skip_and_return_value(&isa, &mut data, ADDR, value),
                            "{insn}"
                        );
                        let new = isa.decode(&data, ADDR as u32).unwrap();
                        assert_eq!(new.opcode(), Opcode::Movi);
                        assert_eq!(
                            new.operands(),
                            [Operand::Ar(reg), Operand::Imm(value as i32)]
                        );
                    }
                    for value in [0x800u64, (-2049i64) as u64, 0x1_0000_0000] {
                        let mut data = code;
                        assert!(!skip_and_return_value(&isa, &mut data, ADDR, value));
                        assert_eq!(data, code);
                    }
                }
            }
            // Not a call.
            assert!(!is_call(&isa, &NOP, ADDR));
            assert!(!skip_and_return_value(&isa, &mut NOP.clone(), ADDR, 0));
        }
    }
}
