//! Decoded instructions and their operands.

use core::fmt;

use crate::{Opcode, SpecialRegister, UserRegister};

/// The maximum number of operands any instruction has.
pub const MAX_OPERANDS: usize = 8;

/// A single instruction operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operand {
    /// An address register, `a0`..`a15`.
    Ar(u8),
    /// A floating point register, `f0`..`f15`.
    Fr(u8),
    /// A boolean register, `b0`..`b15`.
    Br(u8),
    /// A group of consecutive boolean registers, as used by `any4`/`all8`.
    BrGroup {
        /// The first register in the group.
        first: u8,
        /// The number of registers in the group (4 or 8).
        count: u8,
    },
    /// A MAC16 data register, `m0`..`m3`.
    Mr(u8),
    /// An ESP32-S3 128-bit vector register, `q0`..`q7`.
    Qr(u8),
    /// An immediate, already scaled and sign- or zero-extended.
    Imm(i32),
    /// An absolute code address: the target of a branch, jump, call or the
    /// end of a zero-overhead loop.
    Target(u32),
    /// An absolute data address: the literal loaded by `l32r`.
    Literal(u32),
    /// A special register (`rsr`, `wsr`, `xsr`).
    Sr(&'static SpecialRegister),
    /// A user register (`rur`, `wur`).
    Ur(&'static UserRegister),
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Operand::Ar(n) => write!(f, "a{n}"),
            Operand::Fr(n) => write!(f, "f{n}"),
            Operand::Br(n) => write!(f, "b{n}"),
            Operand::BrGroup { first, count } => {
                for i in 0..count {
                    if i != 0 {
                        f.write_str(":")?;
                    }
                    write!(f, "b{}", first + i)?;
                }
                Ok(())
            }
            Operand::Mr(n) => write!(f, "m{n}"),
            Operand::Qr(n) => write!(f, "q{n}"),
            // Matches GNU objdump: small values in decimal, others as 32-bit hex.
            Operand::Imm(v) if -256 < v && v < 256 => write!(f, "{v}"),
            #[allow(clippy::cast_sign_loss)]
            Operand::Imm(v) => write!(f, "{:#x}", v as u32),
            Operand::Target(a) | Operand::Literal(a) => write!(f, "{a:#x}"),
            Operand::Sr(sr) => f.write_str(sr.name),
            Operand::Ur(ur) => f.write_str(ur.name),
        }
    }
}

/// How an instruction affects control flow.
///
/// This is the information a disassembler front end (e.g. a Binary Ninja
/// architecture plugin) needs to build a control flow graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlFlow {
    /// Execution continues with the next instruction.
    Sequential,
    /// A conditional branch to `target`; otherwise falls through.
    Branch {
        /// Branch target.
        target: u32,
    },
    /// An unconditional direct jump (`j`).
    Jump {
        /// Jump target.
        target: u32,
    },
    /// An unconditional indirect jump (`jx`).
    IndirectJump,
    /// A direct call (`call0`, `call4`, `call8`, `call12`).
    Call {
        /// Call target.
        target: u32,
        /// Register window rotation: 0 for `call0`, otherwise 4, 8 or 12.
        window: u8,
    },
    /// An indirect call (`callx0`, `callx4`, ...).
    IndirectCall {
        /// Register window rotation: 0 for `callx0`, otherwise 4, 8 or 12.
        window: u8,
    },
    /// A return from subroutine (`ret`, `retw` and narrow forms).
    Return,
    /// A return from an exception or interrupt handler (`rfe`, `rfi`, ...).
    ExceptionReturn,
    /// A zero-overhead loop setup. Execution continues sequentially; the body
    /// ends at `end`, which is also where execution resumes when the loop is
    /// skipped (`loopnez`/`loopgtz`).
    Loop {
        /// First address after the loop body.
        end: u32,
        /// True for `loopnez`/`loopgtz`, which branch to `end` when the
        /// trip count is not positive.
        conditional: bool,
    },
    /// An instruction that always raises an exception (`ill`, `ill.n`,
    /// `syscall`, `break`, `break.n`).
    Trap,
}

/// A decoded instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Instruction {
    pub(crate) opcode: Opcode,
    pub(crate) address: u32,
    pub(crate) raw: u32,
    pub(crate) len: u8,
    pub(crate) operand_count: u8,
    pub(crate) operands: [Operand; MAX_OPERANDS],
}

impl Instruction {
    /// The operation.
    #[must_use]
    pub const fn opcode(&self) -> Opcode {
        self.opcode
    }

    /// The address the instruction was decoded at.
    #[must_use]
    pub const fn address(&self) -> u32 {
        self.address
    }

    /// Length in bytes: 2 for narrow (Code Density) instructions, 4 for the
    /// ESP32-S3 PIE format, otherwise 3.
    #[must_use]
    #[expect(clippy::len_without_is_empty, reason = "instructions are never empty")]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// The raw instruction word, little-endian, in the low `len()` bytes.
    #[must_use]
    pub const fn raw(&self) -> u32 {
        self.raw
    }

    /// The operands in assembler order.
    #[must_use]
    pub fn operands(&self) -> &[Operand] {
        &self.operands[..self.operand_count as usize]
    }

    /// The mnemonic as written in assembly, including the register name that
    /// special and user register accesses carry in their mnemonic
    /// (`rsr.sar`, `wur.threadptr`).
    #[must_use]
    pub fn mnemonic(&self) -> Mnemonic {
        let register = match self.operands().last() {
            Some(Operand::Sr(sr)) => Some(sr.name),
            Some(Operand::Ur(ur)) => Some(ur.name),
            _ => None,
        };
        Mnemonic {
            opcode: self.opcode,
            register,
        }
    }

    /// The operands written after the mnemonic in assembly: all operands
    /// except a special or user register that is part of [`Self::mnemonic`].
    ///
    /// A front end rendering instruction text (for example, as tokens)
    /// should print [`Self::mnemonic`] followed by these operands.
    #[must_use]
    pub fn text_operands(&self) -> &[Operand] {
        let operands = self.operands();
        match operands.split_last() {
            Some((Operand::Sr(_) | Operand::Ur(_), rest)) => rest,
            _ => operands,
        }
    }

    /// The address of the following instruction.
    #[must_use]
    pub const fn next_address(&self) -> u32 {
        self.address.wrapping_add(self.len as u32)
    }

    fn target(&self) -> u32 {
        match self.operands().last() {
            Some(Operand::Target(t)) => *t,
            _ => unreachable!("{:?} has no target operand", self.opcode),
        }
    }

    /// Classifies the instruction's effect on control flow.
    #[must_use]
    pub fn control_flow(&self) -> ControlFlow {
        use Opcode as O;
        match self.opcode {
            O::Ball
            | O::Bany
            | O::Bbc
            | O::Bbci
            | O::Bbs
            | O::Bbsi
            | O::Beq
            | O::Beqi
            | O::Beqz
            | O::BeqzN
            | O::Bf
            | O::Bge
            | O::Bgei
            | O::Bgeu
            | O::Bgeui
            | O::Bgez
            | O::Blt
            | O::Blti
            | O::Bltu
            | O::Bltui
            | O::Bltz
            | O::Bnall
            | O::Bne
            | O::Bnei
            | O::Bnez
            | O::BnezN
            | O::Bnone
            | O::Bt => ControlFlow::Branch {
                target: self.target(),
            },
            O::J => ControlFlow::Jump {
                target: self.target(),
            },
            O::Jx => ControlFlow::IndirectJump,
            O::Call0 => ControlFlow::Call {
                target: self.target(),
                window: 0,
            },
            O::Call4 => ControlFlow::Call {
                target: self.target(),
                window: 4,
            },
            O::Call8 => ControlFlow::Call {
                target: self.target(),
                window: 8,
            },
            O::Call12 => ControlFlow::Call {
                target: self.target(),
                window: 12,
            },
            O::Callx0 => ControlFlow::IndirectCall { window: 0 },
            O::Callx4 => ControlFlow::IndirectCall { window: 4 },
            O::Callx8 => ControlFlow::IndirectCall { window: 8 },
            O::Callx12 => ControlFlow::IndirectCall { window: 12 },
            O::Ret | O::RetN | O::Retw | O::RetwN => ControlFlow::Return,
            O::Rfe | O::Rfde | O::Rfi | O::Rfwo | O::Rfwu | O::Rfdo | O::Rfdd => {
                ControlFlow::ExceptionReturn
            }
            O::Loop => ControlFlow::Loop {
                end: self.target(),
                conditional: false,
            },
            O::Loopnez | O::Loopgtz => ControlFlow::Loop {
                end: self.target(),
                conditional: true,
            },
            O::Ill | O::IllN | O::Syscall | O::Break | O::BreakN => ControlFlow::Trap,
            _ => ControlFlow::Sequential,
        }
    }
}

impl fmt::Display for Instruction {
    /// Formats the instruction like GNU objdump, except that code and data
    /// addresses are prefixed with `0x`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.mnemonic())?;
        for (i, op) in self.text_operands().iter().enumerate() {
            f.write_str(if i == 0 { " " } else { ", " })?;
            write!(f, "{op}")?;
        }
        Ok(())
    }
}

/// An instruction's mnemonic as written in assembly; see
/// [`Instruction::mnemonic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mnemonic {
    /// The operation.
    pub opcode: Opcode,
    /// The special or user register name appended to the mnemonic, if any.
    pub register: Option<&'static str>,
}

impl fmt::Display for Mnemonic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.opcode.mnemonic())?;
        if let Some(register) = self.register {
            write!(f, ".{register}")?;
        }
        Ok(())
    }
}
