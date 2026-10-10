//! Espressif's RISC-V cores. Their code is analyzed with Binary Ninja's own
//! `rv32gc` architecture; this module adds what the core's architecture
//! does not provide for them: the soft-float calling convention and the
//! instruction patterns the firmware loader looks for. Nothing here knows
//! about Espressif's firmware formats or memory maps (see
//! `crate::espressif`).

use binaryninja::architecture::{ArchitectureExt, CoreArchitecture};
use binaryninja::calling_convention::ConventionBuilder;

/// The name of Binary Ninja's architecture for these cores.
pub const ARCH_NAME: &str = "rv32gc";

/// The soft-float calling convention (see [`register`]).
pub const ILP32: &str = "ilp32";

/// A supported core configuration. All of them decode with `rv32gc`; the
/// configuration says which extensions the code can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Core {
    /// RV32IMC (ESP32-C2, ESP32-C3).
    Rv32imc,
    /// RV32IMAC (ESP32-C6, ESP32-H2, ESP32-C5).
    Rv32imac,
}

impl Core {
    /// The addresses of the functions found by scanning `code`, loaded at
    /// `addr`. RISC-V functions have no marker instruction like Xtensa's
    /// `entry`, so nothing is found yet; this is where the recognition of
    /// ESP-IDF's interrupt vector table belongs.
    pub fn function_starts(self, _addr: u32, _code: &[u8]) -> Vec<u32> {
        Vec::new()
    }

    /// Does `code` start with a stack frame allocation (`addi sp, sp, -n`
    /// or `c.addi16sp sp, -n`)?
    pub fn is_prologue(self, code: &[u8]) -> bool {
        is_frame_alloc(code)
    }
}

/// Is `code` an `addi sp, sp, -n` (32-bit) or `c.addi16sp sp, -n`?
pub fn is_frame_alloc(code: &[u8]) -> bool {
    match code {
        // addi sp, sp, imm: imm[11:0] | rs1=2 | funct3=0 | rd=2 | 0x13,
        // with a negative immediate.
        [b0, b1, b2, b3, ..] if b0 & 3 == 3 => {
            let w = u32::from_le_bytes([*b0, *b1, *b2, *b3]);
            w & 0x000F_FFFF == 0x0001_0113 && w & 0x8000_0000 != 0
        }
        // c.addi16sp: funct3=011 | nzimm[9] | rd=2 | nzimm[4|6|8:7|5] | 01,
        // with nzimm[9] (the sign) set.
        [b0, b1, ..] => {
            let h = u16::from_le_bytes([*b0, *b1]);
            h & 0xEF83 == 0x6101 && h & 0x1000 != 0
        }
        _ => false,
    }
}

/// Registers the `ilp32` calling convention on Binary Ninja's `rv32gc`
/// architecture, if it is loaded.
///
/// The architecture's own `default` convention is the hard-float one
/// (`ilp32f`/`ilp32d`): it passes floating point arguments and results in
/// `fa0..fa7`. Espressif's RV32IMC/RV32IMAC chips have no FPU, and ESP-IDF
/// builds them for `ilp32`, where they go in the integer registers like
/// everything else. Otherwise the convention matches `default`: arguments
/// in `a0..a7`, result in `a0`/`a1`, `s0..s11` callee-saved, `gp` the global
/// pointer.
pub fn register() {
    let Some(arch) = CoreArchitecture::by_name(ARCH_NAME) else {
        tracing::info!("architecture {ARCH_NAME} is not available; RISC-V chips are not supported");
        return;
    };
    if arch.calling_convention_by_name(ILP32).is_some() {
        return;
    }
    let names = |prefix: &str, lo: u8, hi: u8| {
        (lo..=hi)
            .map(|n| format!("{prefix}{n}"))
            .collect::<Vec<_>>()
    };
    let args = names("a", 0, 7);
    let caller_saved = [
        vec!["ra".to_string()],
        names("t", 0, 2),
        args.clone(),
        names("t", 3, 6),
    ]
    .concat();
    let callee_saved = names("s", 0, 11);
    ConventionBuilder::new(&arch)
        .int_arg_registers(&strs(&args))
        .return_int_reg("a0")
        .return_hi_int_reg("a1")
        .caller_saved_registers(&strs(&caller_saved))
        .callee_saved_registers(&strs(&callee_saved))
        .global_pointer_reg("gp")
        .are_argument_registers_used_for_var_args(true)
        .is_eligible_for_heuristics(true)
        .register(ILP32);
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_allocations() {
        // addi sp, sp, -16 / addi sp, sp, -0x90
        assert!(is_frame_alloc(&0xFF01_0113u32.to_le_bytes()));
        assert!(is_frame_alloc(&0xF701_0113u32.to_le_bytes()));
        // addi sp, sp, 16 (an epilogue) and addi a0, sp, -16
        assert!(!is_frame_alloc(&0x0101_0113u32.to_le_bytes()));
        assert!(!is_frame_alloc(&0xFF01_0513u32.to_le_bytes()));
        // c.addi16sp sp, -48 / c.addi16sp sp, -32 / c.addi16sp sp, 32
        assert!(is_frame_alloc(&0x7179u16.to_le_bytes()));
        assert!(is_frame_alloc(&0x713Du16.to_le_bytes()));
        assert!(!is_frame_alloc(&0x6105u16.to_le_bytes()));
        // c.addi sp, -16 (0x1141) is a frame allocation too, but rarely the
        // first instruction; it is not recognized.
        assert!(!is_frame_alloc(&[0x41, 0x11]));
        assert!(!is_frame_alloc(&[0x13]));
    }
}
