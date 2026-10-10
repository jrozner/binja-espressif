//! Espressif's RISC-V cores. Their code is analyzed with Binary Ninja's own
//! `rv32gc` architecture; this module adds what the core's architecture
//! does not provide for them: the soft-float calling convention and the
//! instruction patterns the firmware loader looks for. Nothing here knows
//! about Espressif's firmware formats or memory maps (see
//! `crate::espressif`).

use std::collections::HashMap;

use binaryninja::architecture::{ArchitectureExt, CoreArchitecture, Register};
use binaryninja::binary_view::BinaryView;
use binaryninja::calling_convention::ConventionBuilder;
use binaryninjacore_sys::{
    BNRegisterValue, BNRegisterValueType, BNRegisterValueWithConfidence,
    BNRegisterValueWithConfidenceAndRegister, BNSetUserGlobalPointerValues,
    BNUserGlobalPointerValuesSet,
};

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
    /// Does `code` start with a stack frame allocation (`addi sp, sp, -n`
    /// or `c.addi16sp sp, -n`)?
    pub fn is_prologue(self, code: &[u8]) -> bool {
        is_frame_alloc(code)
    }
}

/// The layout of an interrupt vector table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorTableKind {
    /// `mtvec` in vectored mode (the INTC and PLIC chips): 32 `j handler`
    /// instructions, entry `n` taken for cause `n`. ESP-IDF's
    /// `_vector_table` (`vectors_intc.S`).
    Jumps,
    /// The CLIC's `mtvt`: 48 handler addresses. ESP-IDF's `_mtvt_table`
    /// (`vectors_clic.S`).
    Pointers,
}

/// An interrupt vector table found in code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorTable {
    pub addr: u32,
    pub kind: VectorTableKind,
    /// The distinct handlers, in table order, named where ESP-IDF's
    /// layout makes the handler unambiguous.
    pub handlers: Vec<(u32, Option<&'static str>)>,
}

impl VectorTable {
    /// ESP-IDF's name for the table.
    pub fn name(&self) -> &'static str {
        match self.kind {
            VectorTableKind::Jumps => "_vector_table",
            VectorTableKind::Pointers => "_mtvt_table",
        }
    }

    /// The number of entries.
    pub fn entries(&self) -> usize {
        match self.kind {
            VectorTableKind::Jumps => JUMP_TABLE_ENTRIES,
            VectorTableKind::Pointers => POINTER_TABLE_ENTRIES,
        }
    }
}

const JUMP_TABLE_ENTRIES: usize = 32;
const POINTER_TABLE_ENTRIES: usize = 48;

fn word(code: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(code.get(off..off + 4)?.try_into().ok()?))
}

/// The target of `jal x0, offset` (`j offset`) at `pc`, if `w` is one.
fn jump_target(pc: u32, w: u32) -> Option<u32> {
    if w & 0xFFF != 0x06F {
        return None;
    }
    // imm[20|10:1|11|19:12] in bits 31|30:21|20|19:12.
    let imm = ((w >> 31) << 20)
        | (((w >> 21) & 0x3FF) << 1)
        | (((w >> 20) & 1) << 11)
        | (((w >> 12) & 0xFF) << 12);
    let imm = ((imm << 11) as i32 >> 11) as u32;
    Some(pc.wrapping_add(imm))
}

/// The value that occurs most often in `values`, and how often. Ties go to
/// the earliest value, so the result does not depend on hashing.
fn most_common(values: &[u32]) -> (u32, usize) {
    let mut counts = HashMap::new();
    for &v in values {
        *counts.entry(v).or_insert(0usize) += 1;
    }
    let best = values.iter().map(|v| counts[v]).max().unwrap_or(0);
    let value = values
        .iter()
        .copied()
        .find(|v| counts[v] == best)
        .unwrap_or(0);
    (value, best)
}

/// The distinct values of `values`, in order of first occurrence.
fn distinct(values: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for &v in values {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// Finds ESP-IDF's interrupt vector tables in `code`, loaded at `addr`.
///
/// The handlers are only reached through the table, so call-target
/// analysis cannot find them. To keep code and data that merely look like
/// a table from matching, every handler must be in `code` itself (ESP-IDF
/// puts the tables and their handlers together in IRAM), and the table must
/// have ESP-IDF's shape: a handler shared by most entries, and few others.
pub fn vector_tables(addr: u32, code: &[u8]) -> Vec<VectorTable> {
    let end = addr as u64 + code.len() as u64;
    let inside = |t: u32| (addr as u64..end).contains(&(t as u64)) && t % 2 == 0;
    let mut tables = Vec::new();

    // vectors_intc.S: `.balign 0x100`, 32 `j`s; entry 0 (and the other
    // exception causes) to `_panic_handler`, the rest `_interrupt_handler`.
    for off in aligned_offsets(addr, code.len(), 0x100, JUMP_TABLE_ENTRIES * 4) {
        let pc = |i: usize| addr.wrapping_add((off + 4 * i) as u32);
        let Some(targets) = (0..JUMP_TABLE_ENTRIES)
            .map(|i| jump_target(pc(i), word(code, off + 4 * i)?))
            .collect::<Option<Vec<u32>>>()
        else {
            continue;
        };
        let (common, count) = most_common(&targets);
        if count < JUMP_TABLE_ENTRIES / 2 || distinct(&targets).len() > 8 {
            continue;
        }
        if !targets.iter().all(|&t| inside(t)) {
            continue;
        }
        let handlers = distinct(&targets)
            .into_iter()
            .map(|t| {
                let name = if t == common {
                    Some("_interrupt_handler")
                } else if t == targets[0] {
                    Some("_panic_handler")
                } else {
                    None
                };
                (t, name)
            })
            .collect();
        tables.push(VectorTable {
            addr: pc(0),
            kind: VectorTableKind::Jumps,
            handlers,
        });
    }

    // vectors_clic.S: `.balign 0x40`, 48 words; entries 0..15 (the CLINT
    // interrupts) `_system_int_handler`, most others `_interrupt_handler`.
    for off in aligned_offsets(addr, code.len(), 0x40, POINTER_TABLE_ENTRIES * 4) {
        let Some(targets) = (0..POINTER_TABLE_ENTRIES)
            .map(|i| word(code, off + 4 * i))
            .collect::<Option<Vec<u32>>>()
        else {
            continue;
        };
        let system = targets[0];
        if !targets[..16].iter().all(|&t| t == system) {
            continue;
        }
        let (common, count) = most_common(&targets[16..]);
        if common == system || count < POINTER_TABLE_ENTRIES / 2 || distinct(&targets).len() > 8 {
            continue;
        }
        if !targets.iter().all(|&t| inside(t)) {
            continue;
        }
        let handlers = distinct(&targets)
            .into_iter()
            .map(|t| {
                let name = if t == common {
                    Some("_interrupt_handler")
                } else if t == system {
                    Some("_system_int_handler")
                } else {
                    None
                };
                (t, name)
            })
            .collect();
        tables.push(VectorTable {
            addr: addr.wrapping_add(off as u32),
            kind: VectorTableKind::Pointers,
            handlers,
        });
    }
    tables
}

/// The offsets in a `len`-byte block loaded at `addr` of the `align`-byte
/// aligned addresses with `size` bytes after them in the block.
fn aligned_offsets(addr: u32, len: usize, align: u32, size: usize) -> impl Iterator<Item = usize> {
    let first = ((align - addr % align) % align) as usize;
    (first..(len + 1).saturating_sub(size)).step_by(align as usize)
}

/// The value of `gp` set by ESP-IDF's startup code, found in the first
/// instructions of `code` (the entry point, loaded at `addr`).
///
/// `call_start_cpu0` starts with `la gp, __global_pointer$` under
/// `.option norelax`: `auipc gp, %pcrel_hi(..)` and `addi gp, gp,
/// %pcrel_lo(..)`, possibly after the function's prologue. The bootloader
/// does not set `gp`.
pub fn global_pointer(addr: u32, code: &[u8]) -> Option<u32> {
    let mut off = 0;
    for _ in 0..16 {
        let half = code.get(off..off + 2)?;
        if half[0] & 3 != 3 {
            off += 2;
            continue;
        }
        let w = word(code, off)?;
        // auipc gp, imm: imm[31:12] | rd=3 | 0x17
        if w & 0xFFF == 0x197 {
            let next = word(code, off + 4)?;
            // addi gp, gp, imm: imm[11:0] | rs1=3 | funct3=0 | rd=3 | 0x13
            if next & 0xF_FFFF == 0x1_8193 {
                let lo = ((next as i32) >> 20) as u32;
                let pc = addr.wrapping_add(off as u32);
                return Some(pc.wrapping_add(w & 0xFFFF_F000).wrapping_add(lo));
            }
        }
        off += 4;
    }
    None
}

/// Gives the analysis of `view` the value of `gp` the startup code sets,
/// unless one was set already (by the user, or saved in a database).
///
/// Binary Ninja infers `gp` itself from the function that sets it, but only
/// once that function has been analyzed, after the functions that use `gp`;
/// their accesses stay unresolved until they are reanalyzed. The core only
/// lets the user's values be set, so it is set as one; the Rust API has no
/// wrapper for it, nor for the view's handle, so the core is called directly.
pub fn set_global_pointer(view: &BinaryView, value: u32) {
    let Some(arch) = CoreArchitecture::by_name(ARCH_NAME) else {
        return;
    };
    let Some(gp) = arch.register_by_name("gp") else {
        return;
    };
    let entry = BNRegisterValueWithConfidenceAndRegister {
        reg: gp.id().0,
        value: BNRegisterValueWithConfidence {
            value: BNRegisterValue {
                state: BNRegisterValueType::ConstantPointerValue,
                value: value as i64,
                offset: 0,
                size: 0,
            },
            confidence: 255,
        },
    };
    unsafe {
        if !BNUserGlobalPointerValuesSet(view.handle) {
            BNSetUserGlobalPointerValues(view.handle, &entry, 1);
        }
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

    /// `jal x0, target` at `pc`.
    fn j(pc: u32, target: u32) -> u32 {
        let imm = target.wrapping_sub(pc);
        ((imm >> 20) & 1) << 31
            | ((imm >> 1) & 0x3FF) << 21
            | ((imm >> 11) & 1) << 20
            | ((imm >> 12) & 0xFF) << 12
            | 0x6F
    }

    fn words(ws: &[u32]) -> Vec<u8> {
        ws.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// ESP-IDF's INTC/PLIC table at `addr`, followed by its two handlers.
    fn intc_table(addr: u32) -> Vec<u8> {
        let (panic, interrupt) = (addr + 0x80, addr + 0x82);
        let mut ws: Vec<u32> = (0..32)
            .map(|i| {
                let target = if matches!(i, 0 | 24 | 25) {
                    panic
                } else {
                    interrupt
                };
                j(addr + 4 * i, target)
            })
            .collect();
        ws.push(0x8082_A001); // j . (c.j 0); ret (c.jr ra)
        words(&ws)
    }

    #[test]
    fn jump_targets() {
        // From tests/data/esp32c3.elf: entry 0 of the table at 0x40380000
        // is `j 0x40380080`; `j` backwards decodes too.
        assert_eq!(jump_target(0x4038_0000, 0x0800_006F), Some(0x4038_0080));
        assert_eq!(
            jump_target(0x4038_0010, j(0x4038_0010, 0x4037_0000)),
            Some(0x4037_0000)
        );
        // jal ra, ... is a call, not a vector.
        assert_eq!(jump_target(0x4038_0000, 0x0800_00EF), None);
    }

    #[test]
    fn intc_vector_table() {
        let code = intc_table(0x4038_0000);
        let tables = vector_tables(0x4038_0000, &code);
        assert_eq!(
            tables,
            [VectorTable {
                addr: 0x4038_0000,
                kind: VectorTableKind::Jumps,
                handlers: vec![
                    (0x4038_0080, Some("_panic_handler")),
                    (0x4038_0082, Some("_interrupt_handler")),
                ],
            }]
        );
        assert_eq!(tables[0].name(), "_vector_table");

        // Found at a 256-byte aligned address inside a segment...
        let mut code2 = vec![0u8; 0x100];
        code2.extend(intc_table(0x4038_0100));
        let found = vector_tables(0x4038_0000, &code2);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].addr, 0x4038_0100);
        // ...but not at an unaligned one.
        let mut code3 = vec![0u8; 0x10];
        code3.extend(intc_table(0x4038_0010));
        assert!(vector_tables(0x4038_0000, &code3).is_empty());
        // Handlers outside the segment: not ESP-IDF's layout.
        let far: Vec<u32> = (0..32)
            .map(|i| j(0x4038_0000 + 4 * i, 0x4039_0000))
            .collect();
        assert!(vector_tables(0x4038_0000, &words(&far)).is_empty());
        // A truncated table.
        assert!(vector_tables(0x4038_0000, &code[..124]).is_empty());
    }

    #[test]
    fn clic_vector_table() {
        let base = 0x4080_0000;
        let (system, interrupt, panic) = (base + 0x100, base + 0x104, base + 0x108);
        let ptrs: Vec<u32> = (0..48)
            .map(|i| match i {
                0..16 => system,
                40 => panic,
                _ => interrupt,
            })
            .collect();
        let mut code = vec![0u8; 0x40];
        code.extend(words(&ptrs));
        code.resize(0x110, 0);
        let tables = vector_tables(base, &code);
        assert_eq!(
            tables,
            [VectorTable {
                addr: base + 0x40,
                kind: VectorTableKind::Pointers,
                handlers: vec![
                    (system, Some("_system_int_handler")),
                    (interrupt, Some("_interrupt_handler")),
                    (panic, None),
                ],
            }]
        );
        assert_eq!((tables[0].name(), tables[0].entries()), ("_mtvt_table", 48));
        // 48 copies of one pointer: no distinct system handler.
        let mut same = vec![0u8; 0x40];
        same.extend(words(&[interrupt; 48]));
        same.resize(0x110, 0);
        assert!(vector_tables(base, &same).is_empty());
    }

    #[test]
    fn startup_global_pointer() {
        // tests/data/esp32c3.elf call_start_cpu0: auipc gp, 0xfdc88;
        // addi gp, gp, 992 -> 0x3fc88400.
        let la = words(&[0xFDC8_8197, 0x3E01_8193]);
        assert_eq!(global_pointer(0x4200_0020, &la), Some(0x3FC8_8400));
        // After a prologue: c.addi16sp sp, -48; sw ra, 44(sp).
        let mut code = vec![0x79, 0x71];
        code.extend(words(&[0x0211_2623]));
        code.extend(words(&[0xFDC8_8197, 0x3E01_8193]));
        // (The auipc is 6 bytes later, so gp is too.)
        assert_eq!(global_pointer(0x4200_0020, &code), Some(0x3FC8_8400 + 6));
        // A negative %pcrel_lo: auipc gp, 0x1; addi gp, gp, -2048.
        assert_eq!(
            global_pointer(0x1000, &words(&[0x0000_1197, 0x8001_8193])),
            Some(0x1800)
        );
        // auipc gp without the addi, and an auipc into another register.
        assert_eq!(global_pointer(0, &words(&[0xFDC8_8197, 0x0000_0013])), None);
        assert_eq!(global_pointer(0, &words(&[0xFDC8_8297, 0x3E02_8293])), None);
        // Not within the first 16 instructions.
        let mut late = words(&[0x0000_0013; 16]);
        late.extend(&la);
        assert_eq!(global_pointer(0, &late), None);
    }
}
