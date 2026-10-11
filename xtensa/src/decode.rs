//! Instruction decoding.
//!
//! The decoder walks the opcode tables of the Xtensa ISA reference manual. An
//! encoding is accepted only if the configured [`Isa`] implements it and every
//! field the encoding fixes has its required value, matching what the
//! hardware (and GNU binutils) treat as a legal instruction.

use core::fmt;

use crate::instruction::{Instruction, MAX_OPERANDS, Operand};
use crate::isa::{Access, Features as F, Isa};
use crate::opcode::Opcode as O;

/// Why [`Isa::decode`] could not produce an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodeError {
    /// The input ends before the end of the instruction.
    Truncated,
    /// The bytes do not encode an instruction implemented by the [`Isa`].
    Invalid,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DecodeError::Truncated => "truncated instruction",
            DecodeError::Invalid => "invalid instruction",
        })
    }
}

impl core::error::Error for DecodeError {}

/// Immediate values of the `b4const` operand of `beqi`, `bnei`, `blti`, `bgei`.
const B4CONST: [i32; 16] = [-1, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256];
/// Immediate values of the `b4constu` operand of `bltui`, `bgeui`.
const B4CONSTU: [i32; 16] = [
    32768, 65536, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256,
];

// MAC16 opcodes indexed by the half-word selector in op1[1:0]: ll, hl, lh, hh.
const UMUL_AA: [O; 4] = [O::UmulAaLl, O::UmulAaHl, O::UmulAaLh, O::UmulAaHh];
const MUL_AA: [O; 4] = [O::MulAaLl, O::MulAaHl, O::MulAaLh, O::MulAaHh];
const MULA_AA: [O; 4] = [O::MulaAaLl, O::MulaAaHl, O::MulaAaLh, O::MulaAaHh];
const MULS_AA: [O; 4] = [O::MulsAaLl, O::MulsAaHl, O::MulsAaLh, O::MulsAaHh];
const MUL_AD: [O; 4] = [O::MulAdLl, O::MulAdHl, O::MulAdLh, O::MulAdHh];
const MULA_AD: [O; 4] = [O::MulaAdLl, O::MulaAdHl, O::MulaAdLh, O::MulaAdHh];
const MULS_AD: [O; 4] = [O::MulsAdLl, O::MulsAdHl, O::MulsAdLh, O::MulsAdHh];
const MUL_DA: [O; 4] = [O::MulDaLl, O::MulDaHl, O::MulDaLh, O::MulDaHh];
const MULA_DA: [O; 4] = [O::MulaDaLl, O::MulaDaHl, O::MulaDaLh, O::MulaDaHh];
const MULS_DA: [O; 4] = [O::MulsDaLl, O::MulsDaHl, O::MulsDaLh, O::MulsDaHh];
const MUL_DD: [O; 4] = [O::MulDdLl, O::MulDdHl, O::MulDdLh, O::MulDdHh];
const MULA_DD: [O; 4] = [O::MulaDdLl, O::MulaDdHl, O::MulaDdLh, O::MulaDdHh];
const MULS_DD: [O; 4] = [O::MulsDdLl, O::MulsDdHl, O::MulsDdLh, O::MulsDdHh];
const MULA_DA_LDINC: [O; 4] = [
    O::MulaDaLlLdinc,
    O::MulaDaHlLdinc,
    O::MulaDaLhLdinc,
    O::MulaDaHhLdinc,
];
const MULA_DA_LDDEC: [O; 4] = [
    O::MulaDaLlLddec,
    O::MulaDaHlLddec,
    O::MulaDaLhLddec,
    O::MulaDaHhLddec,
];
const MULA_DD_LDINC: [O; 4] = [
    O::MulaDdLlLdinc,
    O::MulaDdHlLdinc,
    O::MulaDdLhLdinc,
    O::MulaDdHhLdinc,
];
const MULA_DD_LDDEC: [O; 4] = [
    O::MulaDdLlLddec,
    O::MulaDdHlLddec,
    O::MulaDdLhLddec,
    O::MulaDdHhLddec,
];

/// Sign-extends the low `bits` bits of `value`.
#[allow(clippy::cast_possible_wrap)]
const fn sext(value: u32, bits: u32) -> i32 {
    ((value << (32 - bits)) as i32) >> (32 - bits)
}

#[allow(clippy::cast_possible_truncation)]
fn a(n: u32) -> Operand {
    Operand::Ar(n as u8)
}

#[allow(clippy::cast_possible_truncation)]
fn f(n: u32) -> Operand {
    Operand::Fr(n as u8)
}

#[allow(clippy::cast_possible_truncation)]
fn b(n: u32) -> Operand {
    Operand::Br(n as u8)
}

/// MAC16 `m` register.
#[allow(clippy::cast_possible_truncation)]
fn m(n: u32) -> Operand {
    Operand::Mr(n as u8)
}

#[allow(clippy::cast_possible_wrap)]
fn imm(v: u32) -> Operand {
    Operand::Imm(v as i32)
}

fn simm(v: i32) -> Operand {
    Operand::Imm(v)
}

/// Returns `Some(())` if `cond` holds, for use with `?`.
fn require(cond: bool) -> Option<()> {
    cond.then_some(())
}

/// The instruction word and the context needed to decode it.
struct Decoder<'a> {
    isa: &'a Isa,
    pc: u32,
    word: u32,
    len: u8,
}

impl Decoder<'_> {
    // Instruction fields, named as in the ISA reference manual.
    fn op0(&self) -> u32 {
        self.word & 0xf
    }
    fn t(&self) -> u32 {
        (self.word >> 4) & 0xf
    }
    fn s(&self) -> u32 {
        (self.word >> 8) & 0xf
    }
    fn r(&self) -> u32 {
        (self.word >> 12) & 0xf
    }
    fn op1(&self) -> u32 {
        (self.word >> 16) & 0xf
    }
    fn op2(&self) -> u32 {
        (self.word >> 20) & 0xf
    }
    fn n(&self) -> u32 {
        (self.word >> 4) & 0x3
    }
    fn m(&self) -> u32 {
        (self.word >> 6) & 0x3
    }
    fn imm8(&self) -> u32 {
        (self.word >> 16) & 0xff
    }
    fn imm12(&self) -> u32 {
        (self.word >> 12) & 0xfff
    }
    fn imm16(&self) -> u32 {
        (self.word >> 8) & 0xffff
    }
    fn offset18(&self) -> u32 {
        (self.word >> 6) & 0x3_ffff
    }

    /// Requires the ISA to implement `features`.
    fn need(&self, features: F) -> Option<()> {
        require(self.isa.has(features))
    }

    /// A PC-relative code address: `pc + 4 + offset`.
    #[allow(clippy::cast_sign_loss)]
    fn rel(&self, offset: i32) -> Operand {
        Operand::Target(self.pc.wrapping_add(4).wrapping_add(offset as u32))
    }

    /// Target of an 8-bit signed branch offset.
    fn target8(&self) -> Operand {
        self.rel(sext(self.imm8(), 8))
    }

    #[allow(clippy::cast_possible_truncation)]
    fn sr(&self, access: Access) -> Option<Operand> {
        let number = (self.word >> 8) as u8;
        self.isa.special_register(number, access).map(Operand::Sr)
    }

    #[allow(clippy::cast_possible_truncation)]
    fn ur(&self, number: u32) -> Option<Operand> {
        self.isa.user_register(number as u8).map(Operand::Ur)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn emit(&self, opcode: O, ops: &[Operand]) -> Option<Instruction> {
        let mut operands = [Operand::Imm(0); MAX_OPERANDS];
        operands[..ops.len()].copy_from_slice(ops);
        #[allow(clippy::cast_possible_truncation)]
        Some(Instruction {
            opcode,
            address: self.pc,
            raw: self.word,
            len: self.len,
            operand_count: ops.len() as u8,
            operands,
        })
    }

    fn decode(&self) -> Option<Instruction> {
        self.core().or_else(|| self.pie())
    }

    /// ESP32-S3 PIE instructions, which occupy encodings that are reserved
    /// elsewhere: parts of the MAC16 and CUST0/CUST1 spaces and the whole
    /// 4-byte format.
    fn pie(&self) -> Option<Instruction> {
        self.need(F::ESP32S3_PIE)?;
        let (opcode, operands, count) = crate::pie::decode(self.word, self.len)?;
        let operands = &operands[..count];
        // Some PIE instructions move data to or from the FPU's registers.
        if operands.iter().any(|op| matches!(op, Operand::Fr(_))) {
            self.need(F::FP)?;
        }
        self.emit(opcode, operands)
    }

    fn core(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        match self.op0() {
            0x0 => self.qrst(),
            0x1 => {
                // The literal lies at a negative, word-aligned offset from the
                // word-aligned address of the next instruction.
                let base = self.pc.wrapping_add(3) & !3;
                let offset = (0xffff_0000 | self.imm16()) << 2;
                self.emit(
                    O::L32r,
                    &[a(t), Operand::Literal(base.wrapping_add(offset))],
                )
            }
            0x2 => self.lsai(),
            0x3 => self.lsci(),
            0x4 => self.mac16(),
            0x5 => self.calln(),
            0x6 => self.si(),
            0x7 => self.b(),
            0x8 => self.emit(O::L32iN, &[a(t), a(s), imm(r << 2)]),
            0x9 => self.emit(O::S32iN, &[a(t), a(s), imm(r << 2)]),
            0xa => self.emit(O::AddN, &[a(r), a(s), a(t)]),
            0xb => {
                let value = if t == 0 { -1 } else { t.cast_signed() };
                self.emit(O::AddiN, &[a(r), a(s), simm(value)])
            }
            0xc => self.st2(),
            0xd => self.st3(),
            _ => None,
        }
    }

    fn qrst(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        match self.op1() {
            0x0 => self.rst0(),
            0x1 => self.rst1(),
            0x2 => self.rst2(),
            0x3 => self.rst3(),
            0x4 | 0x5 => {
                let shift = s | (self.op1() & 1) << 4;
                self.emit(O::Extui, &[a(r), a(t), imm(shift), imm(self.op2() + 1)])
            }
            0x6 => self.cust0(),
            0x8 => self.lscx(),
            0x9 => self.lsc4(),
            0xa => self.fp0(),
            0xb => self.fp1(),
            0xe => self.esp32_tie(),
            0xf => {
                self.need(F::ESP32_DFP_ACCEL)?;
                self.emit(O::F64cmph, &[a(r), a(s), a(t), imm(self.op2())])
            }
            _ => None,
        }
    }

    fn rst0(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        let rst = [a(r), a(s), a(t)];
        match self.op2() {
            0x0 => self.st0(),
            0x1 => self.emit(O::And, &rst),
            0x2 => self.emit(O::Or, &rst),
            0x3 => self.emit(O::Xor, &rst),
            0x4 => self.st1(),
            0x5 => self.tlb(),
            0x6 => match s {
                0 => self.emit(O::Neg, &[a(r), a(t)]),
                1 => self.emit(O::Abs, &[a(r), a(t)]),
                _ => None,
            },
            0x8 => self.emit(O::Add, &rst),
            0x9 => self.emit(O::Addx2, &rst),
            0xa => self.emit(O::Addx4, &rst),
            0xb => self.emit(O::Addx8, &rst),
            0xc => self.emit(O::Sub, &rst),
            0xd => self.emit(O::Subx2, &rst),
            0xe => self.emit(O::Subx4, &rst),
            0xf => self.emit(O::Subx8, &rst),
            _ => None,
        }
    }

    #[expect(clippy::too_many_lines, reason = "mirrors the ST0 opcode table")]
    fn st0(&self) -> Option<Instruction> {
        let (s, t) = (self.s(), self.t());
        match self.r() {
            0x0 => match t {
                0x0 if s == 0 => self.emit(O::Ill, &[]),
                0x8 => self.emit(O::Ret, &[]),
                0x9 => {
                    self.need(F::WINDOWED)?;
                    self.emit(O::Retw, &[])
                }
                0xa => self.emit(O::Jx, &[a(s)]),
                0xc => self.emit(O::Callx0, &[a(s)]),
                0xd..=0xf => {
                    self.need(F::WINDOWED)?;
                    let op = [O::Callx4, O::Callx8, O::Callx12][(t - 0xd) as usize];
                    self.emit(op, &[a(s)])
                }
                _ => None,
            },
            0x1 => {
                self.need(F::WINDOWED)?;
                self.emit(O::Movsp, &[a(t), a(s)])
            }
            0x2 => {
                require(s == 0)?;
                let op = match t {
                    0x0 => O::Isync,
                    0x1 => O::Rsync,
                    0x2 => O::Esync,
                    0x3 => O::Dsync,
                    0x8 => {
                        self.need(F::EXCEPTIONS)?;
                        O::Excw
                    }
                    0xc => O::Memw,
                    0xd => O::Extw,
                    0xf => O::Nop,
                    _ => return None,
                };
                self.emit(op, &[])
            }
            0x3 => match (t, s) {
                (0, 0) => {
                    self.need(F::EXCEPTIONS)?;
                    self.emit(O::Rfe, &[])
                }
                (0, 2) => {
                    self.need(F::EXCEPTIONS)?;
                    self.emit(O::Rfde, &[])
                }
                (0, 4) => {
                    self.need(F::WINDOWED)?;
                    self.emit(O::Rfwo, &[])
                }
                (0, 5) => {
                    self.need(F::WINDOWED)?;
                    self.emit(O::Rfwu, &[])
                }
                (1, _) => {
                    self.need(F::INTERRUPTS)?;
                    self.emit(O::Rfi, &[imm(s)])
                }
                _ => None,
            },
            0x4 => {
                self.need(F::DEBUG)?;
                self.emit(O::Break, &[imm(s), imm(t)])
            }
            0x5 => {
                self.need(F::EXCEPTIONS)?;
                match (s, t) {
                    (0, 0) => self.emit(O::Syscall, &[]),
                    (1, 0) => self.emit(O::Simcall, &[]),
                    _ => None,
                }
            }
            0x6 => {
                self.need(F::INTERRUPTS)?;
                self.emit(O::Rsil, &[a(t), imm(s)])
            }
            0x7 => match t {
                0x0 => {
                    self.need(F::INTERRUPTS)?;
                    self.emit(O::Waiti, &[imm(s)])
                }
                0xe => {
                    self.need(F::DEBUG_DDR)?;
                    self.emit(O::Lddr32P, &[a(s)])
                }
                0xf => {
                    self.need(F::DEBUG_DDR)?;
                    self.emit(O::Sddr32P, &[a(s)])
                }
                _ => None,
            },
            r @ 0x8..=0xb => {
                self.need(F::BOOLEAN)?;
                let (op, count) =
                    [(O::Any4, 4), (O::All4, 4), (O::Any8, 8), (O::All8, 8)][(r - 8) as usize];
                // The group is named by its first register; the low bits of s
                // are ignored, as by objdump.
                #[allow(clippy::cast_possible_truncation)]
                let group = Operand::BrGroup {
                    first: (s & !(count - 1)) as u8,
                    count: count as u8,
                };
                self.emit(op, &[b(t), group])
            }
            _ => None,
        }
    }

    fn st1(&self) -> Option<Instruction> {
        let (s, t) = (self.s(), self.t());
        match self.r() {
            r @ 0x0..=0x3 => {
                require(t == 0)?;
                let op = [O::Ssr, O::Ssl, O::Ssa8l, O::Ssa8b][r as usize];
                self.emit(op, &[a(s)])
            }
            0x4 => {
                require(t & 0xe == 0)?;
                self.emit(O::Ssai, &[imm(s | (t & 1) << 4)])
            }
            0x6 => {
                self.need(F::EXTERNAL_REGS)?;
                self.emit(O::Rer, &[a(t), a(s)])
            }
            0x7 => {
                self.need(F::EXTERNAL_REGS)?;
                self.emit(O::Wer, &[a(t), a(s)])
            }
            0x8 => {
                self.need(F::WINDOWED)?;
                require(s == 0)?;
                self.emit(O::Rotw, &[simm(sext(t, 4))])
            }
            0xe => {
                self.need(F::NSA)?;
                self.emit(O::Nsa, &[a(t), a(s)])
            }
            0xf => {
                self.need(F::NSA)?;
                self.emit(O::Nsau, &[a(t), a(s)])
            }
            _ => None,
        }
    }

    fn tlb(&self) -> Option<Instruction> {
        self.need(F::TLB)?;
        let (s, t) = (self.s(), self.t());
        let op = match self.r() {
            0x3 => O::Ritlb0,
            0x4 | 0xc => {
                require(t == 0)?;
                let op = if self.r() == 0x4 { O::Iitlb } else { O::Idtlb };
                return self.emit(op, &[a(s)]);
            }
            0x5 => O::Pitlb,
            0x6 => O::Witlb,
            0x7 => O::Ritlb1,
            0xb => O::Rdtlb0,
            0xd => O::Pdtlb,
            0xe => O::Wdtlb,
            0xf => O::Rdtlb1,
            _ => return None,
        };
        self.emit(op, &[a(t), a(s)])
    }

    fn rst1(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        let op2 = self.op2();
        match op2 {
            0x0 | 0x1 => {
                let field = t | (op2 & 1) << 4;
                self.emit(O::Slli, &[a(r), a(s), imm(32 - field)])
            }
            0x2 | 0x3 => self.emit(O::Srai, &[a(r), a(t), imm(s | (op2 & 1) << 4)]),
            0x4 => self.emit(O::Srli, &[a(r), a(t), imm(s)]),
            0x6 => self.emit(O::Xsr, &[a(t), self.sr(Access::Exchange)?]),
            0x8 => self.emit(O::Src, &[a(r), a(s), a(t)]),
            0x9 => {
                require(s == 0)?;
                self.emit(O::Srl, &[a(r), a(t)])
            }
            0xa => {
                require(t == 0)?;
                self.emit(O::Sll, &[a(r), a(s)])
            }
            0xb => {
                require(s == 0)?;
                self.emit(O::Sra, &[a(r), a(t)])
            }
            0xc => {
                self.need(F::MUL16)?;
                self.emit(O::Mul16u, &[a(r), a(s), a(t)])
            }
            0xd => {
                self.need(F::MUL16)?;
                self.emit(O::Mul16s, &[a(r), a(s), a(t)])
            }
            0xf if r == 0xe => {
                self.need(F::DEBUG)?;
                match t {
                    0 => self.emit(O::Rfdo, &[imm(s)]),
                    1 => self.emit(O::Rfdd, &[]),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn rst2(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        let (op, features) = match self.op2() {
            0x0 => (O::Andb, F::BOOLEAN),
            0x1 => (O::Andbc, F::BOOLEAN),
            0x2 => (O::Orb, F::BOOLEAN),
            0x3 => (O::Orbc, F::BOOLEAN),
            0x4 => (O::Xorb, F::BOOLEAN),
            0x6 => (O::Saltu, F::SALT),
            0x7 => (O::Salt, F::SALT),
            0x8 => (O::Mull, F::MUL32),
            0xa => (O::Muluh, F::MUL32_HIGH),
            0xb => (O::Mulsh, F::MUL32_HIGH),
            0xc => (O::Quou, F::DIV32),
            0xd => (O::Quos, F::DIV32),
            0xe => (O::Remu, F::DIV32),
            0xf => (O::Rems, F::DIV32),
            _ => return None,
        };
        self.need(features)?;
        if features == F::BOOLEAN {
            self.emit(op, &[b(r), b(s), b(t)])
        } else {
            self.emit(op, &[a(r), a(s), a(t)])
        }
    }

    fn rst3(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        let rst = [a(r), a(s), a(t)];
        match self.op2() {
            0x0 => self.emit(O::Rsr, &[a(t), self.sr(Access::Read)?]),
            0x1 => self.emit(O::Wsr, &[a(t), self.sr(Access::Write)?]),
            0x2 => {
                self.need(F::SEXT)?;
                self.emit(O::Sext, &[a(r), a(s), imm(t + 7)])
            }
            0x3 => {
                self.need(F::CLAMPS)?;
                self.emit(O::Clamps, &[a(r), a(s), imm(t + 7)])
            }
            op2 @ 0x4..=0x7 => {
                self.need(F::MINMAX)?;
                self.emit([O::Min, O::Max, O::Minu, O::Maxu][(op2 - 4) as usize], &rst)
            }
            0x8 => self.emit(O::Moveqz, &rst),
            0x9 => self.emit(O::Movnez, &rst),
            0xa => self.emit(O::Movltz, &rst),
            0xb => self.emit(O::Movgez, &rst),
            0xc => {
                self.need(F::BOOLEAN)?;
                self.emit(O::Movf, &[a(r), a(s), b(t)])
            }
            0xd => {
                self.need(F::BOOLEAN)?;
                self.emit(O::Movt, &[a(r), a(s), b(t)])
            }
            0xe => self.emit(O::Rur, &[a(r), self.ur(s << 4 | t)?]),
            0xf => self.emit(O::Wur, &[a(t), self.ur(r << 4 | s)?]),
            _ => None,
        }
    }

    fn lscx(&self) -> Option<Instruction> {
        self.need(F::FP)?;
        let op = match self.op2() {
            0x0 => O::Lsx,
            0x1 => O::Lsxp,
            0x4 => O::Ssx,
            0x5 => O::Ssxp,
            _ => return None,
        };
        self.emit(op, &[f(self.r()), a(self.s()), a(self.t())])
    }

    fn lsc4(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        match self.op2() {
            0x0 | 0x4 => {
                self.need(F::WINDOWED)?;
                let op = if self.op2() == 0 { O::L32e } else { O::S32e };
                self.emit(op, &[a(t), a(s), simm(r.cast_signed() * 4 - 64)])
            }
            0x5 => {
                self.need(F::S32NB)?;
                self.emit(O::S32nb, &[a(t), a(s), imm(r << 2)])
            }
            _ => None,
        }
    }

    fn fp0(&self) -> Option<Instruction> {
        self.need(F::FP)?;
        let (r, s, t) = (self.r(), self.s(), self.t());
        match self.op2() {
            op2 @ (0x0..=0x2 | 0x4..=0x7) => {
                let op = match op2 {
                    0x0 => O::AddS,
                    0x1 => O::SubS,
                    0x2 => O::MulS,
                    0x4 => O::MaddS,
                    0x5 => O::MsubS,
                    0x6 => O::MaddnS,
                    _ => O::DivnS,
                };
                self.emit(op, &[f(r), f(s), f(t)])
            }
            op2 @ (0x8..=0xb | 0xe) => {
                let op = match op2 {
                    0x8 => O::RoundS,
                    0x9 => O::TruncS,
                    0xa => O::FloorS,
                    0xb => O::CeilS,
                    _ => O::UtruncS,
                };
                self.emit(op, &[a(r), f(s), imm(t)])
            }
            0xc => self.emit(O::FloatS, &[f(r), a(s), imm(t)]),
            0xd => self.emit(O::UfloatS, &[f(r), a(s), imm(t)]),
            0xf => match t {
                0x3 => self.emit(O::ConstS, &[f(r), imm(s)]),
                0x4 => self.emit(O::Rfr, &[a(r), f(s)]),
                0x5 => self.emit(O::Wfr, &[f(r), a(s)]),
                _ => {
                    let op = match t {
                        0x0 => O::MovS,
                        0x1 => O::AbsS,
                        0x6 => O::NegS,
                        0x7 => O::Div0S,
                        0x8 => O::Recip0S,
                        0x9 => O::Sqrt0S,
                        0xa => O::Rsqrt0S,
                        0xb => O::Nexp01S,
                        0xc => O::MksadjS,
                        0xd => O::MkdadjS,
                        0xe => O::AddexpS,
                        0xf => O::AddexpmS,
                        _ => return None,
                    };
                    self.emit(op, &[f(r), f(s)])
                }
            },
            _ => None,
        }
    }

    fn fp1(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        match self.op2() {
            0x0 => {
                self.need(F::ESP32_DFP_ACCEL)?;
                if r == 0xc && s & 0xe == 0xe {
                    self.emit(O::Rf64r, &[a(t), imm(s & 1)])
                } else if r & 0xe == 0xe {
                    self.emit(O::Wf64r, &[a(s), a(t), imm(r & 1)])
                } else {
                    None
                }
            }
            // Compares write a boolean register.
            op2 @ 0x1..=0x7 => {
                self.need(F::FP | F::BOOLEAN)?;
                let op = [O::UnS, O::OeqS, O::UeqS, O::OltS, O::UltS, O::OleS, O::UleS]
                    [(op2 - 1) as usize];
                self.emit(op, &[b(r), f(s), f(t)])
            }
            op2 @ 0x8..=0xb => {
                self.need(F::FP)?;
                let op = [O::MoveqzS, O::MovnezS, O::MovltzS, O::MovgezS][(op2 - 8) as usize];
                self.emit(op, &[f(r), f(s), a(t)])
            }
            0xc => {
                self.need(F::FP | F::BOOLEAN)?;
                self.emit(O::MovfS, &[f(r), f(s), b(t)])
            }
            0xd => {
                self.need(F::FP | F::BOOLEAN)?;
                self.emit(O::MovtS, &[f(r), f(s), b(t)])
            }
            0xe => {
                self.need(F::ESP32_DFP_ACCEL)?;
                self.emit(O::F64cmpl, &[a(r), a(s), a(t)])
            }
            _ => {
                self.need(F::ESP32_DFP_ACCEL)?;
                let op = if r & 8 == 0 { O::F64addc } else { O::F64subc };
                self.emit(op, &[a(t), a(s), imm((r >> 1) & 3), imm(r & 1)])
            }
        }
    }

    /// QRST op1 = 0x6 (CUST0): ESP32-S2 dedicated GPIO instructions.
    fn cust0(&self) -> Option<Instruction> {
        self.need(F::ESP32S2_GPIO)?;
        require(self.op2() == 0)?;
        let (s, t) = (self.s(), self.t());
        match self.r() {
            0x0 => self.emit(O::ClrBitGpioOut, &[imm(s << 4 | t)]),
            0x1 => self.emit(O::SetBitGpioOut, &[imm(s << 4 | t)]),
            0x2 => self.emit(O::WrMaskGpioOut, &[a(s), a(t)]),
            0x3 if s == 0 => self.emit(O::GetGpioIn, &[a(t)]),
            _ => None,
        }
    }

    /// QRST op1 = 0xe: ESP32-specific TIE instructions.
    fn esp32_tie(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        match self.op2() {
            0x0 => match r {
                0x0 if s == 0 => {
                    self.need(F::ESP32_EXPSTATE)?;
                    self.emit(O::ReadImpwire, &[a(t)])
                }
                0x1 if s < 4 => {
                    self.need(F::ESP32_EXPSTATE)?;
                    let op = if s < 2 {
                        O::SetbExpstate
                    } else {
                        O::ClrbExpstate
                    };
                    self.emit(op, &[imm(t | (s & 1) << 4)])
                }
                0x2 => {
                    self.need(F::ESP32_EXPSTATE)?;
                    self.emit(O::WrmskExpstate, &[a(t), a(s)])
                }
                0xd => {
                    self.need(F::ESP32_DFP_ACCEL)?;
                    self.emit(O::F64sig, &[a(t), a(s)])
                }
                _ => None,
            },
            op2 => {
                self.need(F::ESP32_DFP_ACCEL)?;
                let rst = [a(r), a(s), a(t)];
                match op2 {
                    0x1 => self.emit(O::F64sexp, &rst),
                    0x2 | 0x3 => self.emit(O::F64norm, &[rst[0], rst[1], rst[2], imm(op2 & 1)]),
                    0x4..=0x7 => self.emit(O::F64rnd, &[rst[0], rst[1], rst[2], imm(op2 & 3)]),
                    _ => self.emit(
                        O::F64iter,
                        &[rst[0], rst[1], rst[2], imm(op2 & 3), imm((op2 >> 2) & 1)],
                    ),
                }
            }
        }
    }

    fn lsai(&self) -> Option<Instruction> {
        let (s, t) = (self.s(), self.t());
        let imm8 = self.imm8();
        let (op, scale) = match self.r() {
            0x0 => (O::L8ui, 0),
            0x1 => (O::L16ui, 1),
            0x2 => (O::L32i, 2),
            0x4 => (O::S8i, 0),
            0x5 => (O::S16i, 1),
            0x6 => (O::S32i, 2),
            0x9 => (O::L16si, 1),
            0xa => {
                let value = sext(s << 8 | imm8, 12);
                return self.emit(O::Movi, &[a(t), simm(value)]);
            }
            0xb => {
                self.need(F::MP_SYNC)?;
                (O::L32ai, 2)
            }
            0xc => return self.emit(O::Addi, &[a(t), a(s), simm(sext(imm8, 8))]),
            0xd => return self.emit(O::Addmi, &[a(t), a(s), simm(sext(imm8, 8) << 8)]),
            0xe => {
                self.need(F::S32C1I)?;
                (O::S32c1i, 2)
            }
            0xf => {
                self.need(F::MP_SYNC)?;
                (O::S32ri, 2)
            }
            _ => return None,
        };
        self.emit(op, &[a(t), a(s), imm(imm8 << scale)])
    }

    fn lsci(&self) -> Option<Instruction> {
        self.need(F::FP)?;
        let op = match self.r() {
            0x0 => O::Lsi,
            0x4 => O::Ssi,
            0x8 => O::Lsip,
            0xc => O::Ssip,
            _ => return None,
        };
        self.emit(op, &[f(self.t()), a(self.s()), imm(self.imm8() << 2)])
    }

    fn mac16(&self) -> Option<Instruction> {
        self.need(F::MAC16)?;
        let (r, s, t) = (self.r(), self.s(), self.t());
        let (op1, op2) = (self.op1(), self.op2());
        let half = (op1 & 3) as usize;
        // Operand fields that only encode m0/m1 (x) or m2/m3 (y).
        let mx = m((r >> 2) & 1);
        let my = m(2 + ((t >> 2) & 1));
        let y_ok = t & 0xb == 0;
        match (op2, op1 >> 2) {
            // Multiply-accumulate with parallel load and address update.
            (0x0 | 0x1 | 0x4 | 0x5, 2) => {
                require(r & 8 == 0)?;
                let mw = m(r & 3);
                match op2 {
                    0x0 | 0x1 => {
                        require(y_ok)?;
                        let op = if op2 == 0 {
                            MULA_DD_LDINC
                        } else {
                            MULA_DD_LDDEC
                        }[half];
                        self.emit(op, &[mw, a(s), mx, my])
                    }
                    _ => {
                        let op = if op2 == 4 {
                            MULA_DA_LDINC
                        } else {
                            MULA_DA_LDDEC
                        }[half];
                        self.emit(op, &[mw, a(s), mx, a(t)])
                    }
                }
            }
            (0x2, kind @ 1..=3) => {
                require(s == 0 && r & 0xb == 0 && y_ok)?;
                let op = [MUL_DD, MULA_DD, MULS_DD][(kind - 1) as usize][half];
                self.emit(op, &[mx, my])
            }
            (0x3, kind @ 1..=3) => {
                require(r == 0 && y_ok)?;
                let op = [MUL_AD, MULA_AD, MULS_AD][(kind - 1) as usize][half];
                self.emit(op, &[a(s), my])
            }
            (0x6, kind @ 1..=3) => {
                require(s == 0 && r & 0xb == 0)?;
                let op = [MUL_DA, MULA_DA, MULS_DA][(kind - 1) as usize][half];
                self.emit(op, &[mx, a(t)])
            }
            (0x7, kind) => {
                require(r == 0)?;
                let op = [UMUL_AA, MUL_AA, MULA_AA, MULS_AA][kind as usize][half];
                self.emit(op, &[a(s), a(t)])
            }
            (0x8 | 0x9, _) if op1 == 0 => {
                require(r & 0xc == 0 && t == 0)?;
                let op = if op2 == 8 { O::Ldinc } else { O::Lddec };
                self.emit(op, &[m(r & 3), a(s)])
            }
            _ => None,
        }
    }

    fn calln(&self) -> Option<Instruction> {
        let n = self.n();
        if n != 0 {
            self.need(F::WINDOWED)?;
        }
        let op = [O::Call0, O::Call4, O::Call8, O::Call12][n as usize];
        // Calls target a word-aligned address relative to the aligned PC.
        let offset = (sext(self.offset18(), 18) << 2).cast_unsigned();
        let target = (self.pc & !3).wrapping_add(offset).wrapping_add(4);
        self.emit(op, &[Operand::Target(target)])
    }

    fn si(&self) -> Option<Instruction> {
        let (r, s) = (self.r(), self.s());
        let m = self.m() as usize;
        match self.n() {
            0 => self.emit(O::J, &[self.rel(sext(self.offset18(), 18))]),
            1 => {
                let op = [O::Beqz, O::Bnez, O::Bltz, O::Bgez][m];
                self.emit(op, &[a(s), self.rel(sext(self.imm12(), 12))])
            }
            2 => {
                let op = [O::Beqi, O::Bnei, O::Blti, O::Bgei][m];
                self.emit(op, &[a(s), simm(B4CONST[r as usize]), self.target8()])
            }
            _ => match m {
                0 => {
                    self.need(F::WINDOWED)?;
                    self.emit(O::Entry, &[a(s), imm(self.imm12() << 3)])
                }
                1 => match r {
                    0x0 | 0x1 => {
                        self.need(F::BOOLEAN)?;
                        let op = if r == 0 { O::Bf } else { O::Bt };
                        self.emit(op, &[b(s), self.target8()])
                    }
                    0x8..=0xa => {
                        self.need(F::LOOP)?;
                        let op = [O::Loop, O::Loopnez, O::Loopgtz][(r - 8) as usize];
                        // Loop offsets are unsigned.
                        self.emit(op, &[a(s), self.rel(self.imm8().cast_signed())])
                    }
                    _ => None,
                },
                _ => {
                    let op = if m == 2 { O::Bltui } else { O::Bgeui };
                    self.emit(op, &[a(s), simm(B4CONSTU[r as usize]), self.target8()])
                }
            },
        }
    }

    fn b(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        let op = match r {
            0x6 | 0x7 | 0xe | 0xf => {
                let op = if r < 8 { O::Bbci } else { O::Bbsi };
                return self.emit(op, &[a(s), imm(t | (r & 1) << 4), self.target8()]);
            }
            0x0 => O::Bnone,
            0x1 => O::Beq,
            0x2 => O::Blt,
            0x3 => O::Bltu,
            0x4 => O::Ball,
            0x5 => O::Bbc,
            0x8 => O::Bany,
            0x9 => O::Bne,
            0xa => O::Bge,
            0xb => O::Bgeu,
            0xc => O::Bnall,
            _ => O::Bbs,
        };
        self.emit(op, &[a(s), a(t), self.target8()])
    }

    fn st2(&self) -> Option<Instruction> {
        let (r, s, t) = (self.r(), self.s(), self.t());
        if t & 8 == 0 {
            // 7-bit immediate covering -32..=95.
            let value = (r | (t & 7) << 4).cast_signed();
            let value = if value >= 96 { value - 128 } else { value };
            self.emit(O::MoviN, &[a(s), simm(value)])
        } else {
            let op = if t & 4 == 0 { O::BeqzN } else { O::BnezN };
            // 6-bit unsigned (forward-only) offset.
            self.emit(op, &[a(s), self.rel((r | (t & 3) << 4).cast_signed())])
        }
    }

    fn st3(&self) -> Option<Instruction> {
        let (s, t) = (self.s(), self.t());
        match self.r() {
            0x0 => self.emit(O::MovN, &[a(t), a(s)]),
            0xf => match t {
                0x0 => self.emit(O::RetN, &[]),
                0x1 => {
                    self.need(F::WINDOWED)?;
                    self.emit(O::RetwN, &[])
                }
                0x2 => {
                    self.need(F::DEBUG)?;
                    self.emit(O::BreakN, &[imm(s)])
                }
                0x3 if s == 0 => self.emit(O::NopN, &[]),
                0x6 if s == 0 => self.emit(O::IllN, &[]),
                _ => None,
            },
            _ => None,
        }
    }
}

impl Isa {
    /// Returns the length of the instruction starting with `first_byte`, or
    /// `None` if no instruction of this ISA starts with that byte.
    ///
    /// The length of an Xtensa instruction is determined by the low nibble
    /// (`op0`) of its first byte alone: 2 for narrow (Code Density)
    /// instructions, 4 for the ESP32-S3 PIE format, otherwise 3.
    #[must_use]
    pub const fn instruction_length(&self, first_byte: u8) -> Option<usize> {
        match first_byte & 0xf {
            0x0..=0x7 => Some(3),
            0x8..=0xd if self.has(F::DENSITY) => Some(2),
            0xe | 0xf if self.has(F::ESP32S3_PIE) => Some(4),
            _ => None,
        }
    }

    /// Decodes the instruction at the start of `bytes`, which is located at
    /// `address`.
    ///
    /// Trailing bytes beyond the instruction are ignored.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Truncated`] if `bytes` is shorter than the
    /// instruction and [`DecodeError::Invalid`] if the bytes do not encode an
    /// instruction implemented by this ISA.
    pub fn decode(&self, bytes: &[u8], address: u32) -> Result<Instruction, DecodeError> {
        let &first = bytes.first().ok_or(DecodeError::Truncated)?;
        let len = self.instruction_length(first).ok_or(DecodeError::Invalid)?;
        let bytes = bytes.get(..len).ok_or(DecodeError::Truncated)?;
        let word = bytes
            .iter()
            .rev()
            .fold(0u32, |word, &byte| word << 8 | u32::from(byte));
        #[allow(clippy::cast_possible_truncation)]
        let decoder = Decoder {
            isa: self,
            pc: address,
            word,
            len: len as u8,
        };
        decoder.decode().ok_or(DecodeError::Invalid)
    }

    /// Returns an iterator that linearly disassembles `bytes`, the first of
    /// which is located at `address`.
    ///
    /// Like objdump, the iterator skips a single byte after an invalid
    /// encoding. It stops after reporting a truncated instruction at the end of
    /// the input.
    #[must_use]
    pub fn disassemble<'a>(&'a self, bytes: &'a [u8], address: u32) -> Disassembly<'a> {
        Disassembly {
            isa: self,
            bytes,
            address,
        }
    }
}

/// Iterator returned by [`Isa::disassemble`].
#[derive(Debug, Clone)]
pub struct Disassembly<'a> {
    isa: &'a Isa,
    bytes: &'a [u8],
    address: u32,
}

impl Iterator for Disassembly<'_> {
    /// The address of each decode attempt and its result.
    type Item = (u32, Result<Instruction, DecodeError>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.bytes.is_empty() {
            return None;
        }
        let address = self.address;
        let result = self.isa.decode(self.bytes, address);
        let advance = match &result {
            Ok(insn) => insn.len(),
            Err(DecodeError::Invalid) => 1,
            Err(DecodeError::Truncated) => self.bytes.len(),
        };
        self.bytes = &self.bytes[advance..];
        #[allow(clippy::cast_possible_truncation)]
        let advance = advance as u32;
        self.address = address.wrapping_add(advance);
        Some((address, result))
    }
}

impl core::iter::FusedIterator for Disassembly<'_> {}
