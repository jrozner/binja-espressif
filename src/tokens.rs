//! Instruction text, in GNU objdump syntax.

use binaryninja::disassembly::{InstructionTextToken, InstructionTextTokenKind as K};
use xtensa::{Instruction, Operand};

/// Mnemonics are padded to this width so operands line up.
const MNEMONIC_WIDTH: usize = 12;

pub fn tokens(insn: &Instruction) -> Vec<InstructionTextToken> {
    let mnemonic = insn.mnemonic().to_string();
    let operands = insn.text_operands();
    let mut out = Vec::with_capacity(2 + operands.len() * 2);
    let pad = MNEMONIC_WIDTH.saturating_sub(mnemonic.len()).max(1);
    out.push(InstructionTextToken::new(mnemonic, K::Instruction));
    if operands.is_empty() {
        return out;
    }
    out.push(InstructionTextToken::new(" ".repeat(pad), K::Text));
    for (i, op) in operands.iter().enumerate() {
        if i != 0 {
            out.push(InstructionTextToken::new(", ", K::OperandSeparator));
        }
        out.push(operand_token(op));
    }
    out
}

fn operand_token(op: &Operand) -> InstructionTextToken {
    let text = op.to_string();
    match *op {
        Operand::Imm(v) => InstructionTextToken::new(
            text,
            K::Integer {
                value: v as i64 as u64,
                size: None,
                operand: None,
            },
        ),
        Operand::Target(a) => InstructionTextToken::new(
            text,
            K::CodeRelativeAddress {
                value: a as u64,
                size: Some(4),
                operand: None,
            },
        ),
        Operand::Literal(a) => InstructionTextToken::new(
            text,
            K::PossibleAddress {
                value: a as u64,
                size: Some(4),
                operand: None,
            },
        ),
        _ => InstructionTextToken::new(text, K::Register),
    }
}
