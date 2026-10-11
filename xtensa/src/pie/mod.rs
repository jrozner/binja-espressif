//! Table-driven decoding of the ESP32-S3 Processor Instruction Extensions
//! (PIE): the `ee.*` vector instructions and `ld.qr`/`st.qr`/`mv.qr`.
//!
//! There are over two hundred of these instructions with irregular,
//! scattered operand fields (the 4-byte format permutes its bits), so
//! rather than hand-written decoders they are described by a table generated
//! from the reference toolchain (see `scripts/oracle/gen_esp32s3_pie.py`).

mod esp32s3;

use crate::instruction::{MAX_OPERANDS, Operand};
use crate::opcode::Opcode;

/// A run of instruction bits contributing `value * scale` to an operand.
pub(crate) struct Field {
    pub lsb: u8,
    pub width: u8,
    pub scale: i32,
}

/// The kind of value an operand denotes.
pub(crate) enum Kind {
    Ar,
    Fr,
    Qr,
    Imm,
}

/// An operand: `base` plus the sum of its fields.
pub(crate) struct Spec {
    pub kind: Kind,
    pub base: i32,
    pub fields: &'static [Field],
}

/// One instruction: it matches words where `word & mask == value`.
pub(crate) struct Pie {
    pub opcode: Opcode,
    pub len: u8,
    pub mask: u32,
    pub value: u32,
    pub operands: &'static [Spec],
}

impl Spec {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn decode(&self, word: u32) -> Operand {
        let value = self.fields.iter().fold(self.base, |acc, f| {
            let bits = (word >> f.lsb) & ((1 << f.width) - 1);
            acc + bits as i32 * f.scale
        });
        match self.kind {
            Kind::Ar => Operand::Ar(value as u8),
            Kind::Fr => Operand::Fr(value as u8),
            Kind::Qr => Operand::Qr(value as u8),
            Kind::Imm => Operand::Imm(value),
        }
    }
}

/// Decodes an ESP32-S3 PIE instruction of `len` bytes, returning its opcode
/// and operands.
pub(crate) fn decode(word: u32, len: u8) -> Option<(Opcode, [Operand; MAX_OPERANDS], usize)> {
    let insn = esp32s3::TABLE
        .iter()
        .find(|insn| insn.len == len && word & insn.mask == insn.value)?;
    let mut operands = [Operand::Imm(0); MAX_OPERANDS];
    for (slot, spec) in operands.iter_mut().zip(insn.operands) {
        *slot = spec.decode(word);
    }
    Some((insn.opcode, operands, insn.operands.len()))
}
