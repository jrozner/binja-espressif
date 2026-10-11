//! Espressif's RISC-V cores. Their code is analyzed with Binary Ninja's own
//! `rv32gc` architecture; this module adds what the core's architecture
//! does not provide for them: the soft-float calling convention and the
//! instruction patterns the firmware loader looks for. Nothing here knows
//! about Espressif's firmware formats or memory maps (see
//! `crate::espressif`).

use std::collections::HashMap;

use binaryninja::architecture::{ArchitectureExt, CoreArchitecture, Register, RegisterId};
use binaryninja::binary_view::{BinaryView, MetadataStoreFlags};
use binaryninja::calling_convention::{
    CallingConvention, CoreCallingConvention, register_calling_convention,
};
use binaryninja::types::{
    FunctionParameter, Type, ValueLocation, ValueLocationComponent, ValueLocationSource,
};
use binaryninja::variable::{Variable, VariableSourceType};
use binaryninjacore_sys::{
    BNFreeRegisterValueWithConfidenceAndRegisterList, BNGetUserGlobalPointerValues,
    BNRegisterValue, BNRegisterValueType, BNRegisterValueWithConfidence,
    BNRegisterValueWithConfidenceAndRegister, BNSetUserGlobalPointerValues,
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
    let inside = |t: u32| (addr as u64..end).contains(&(t as u64)) && t.is_multiple_of(2);
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

/// The view metadata key recording that [`set_global_pointer`] has run on a
/// view (saved with the database too).
const GLOBAL_POINTER_SET: &str = "espressif.global_pointer_set";

/// The user global pointer values `existing` with `entry` added, or `None`
/// if they are to be left alone because `existing` already has a value for
/// `entry`'s register. Values for other registers are kept as they are.
fn with_global_pointer(
    existing: &[BNRegisterValueWithConfidenceAndRegister],
    entry: BNRegisterValueWithConfidenceAndRegister,
) -> Option<Vec<BNRegisterValueWithConfidenceAndRegister>> {
    if existing.iter().any(|v| v.reg == entry.reg) {
        return None;
    }
    let mut values = existing.to_vec();
    values.push(entry);
    Some(values)
}

/// Gives the analysis of `view` the value of `gp` the startup code sets,
/// when the file is first loaded and `gp` has no value yet; user values for
/// other registers are kept.
///
/// Binary Ninja infers `gp` itself from the function that sets it, but only
/// once that function has been analyzed, after the functions that use `gp`;
/// their accesses stay unresolved until they are reanalyzed. The core only
/// lets the user's values be set, so it is set as one; the Rust API has no
/// wrapper for it, nor for the view's handle, so the core is called directly.
///
/// It is set at most once, so that a value the user changes or clears
/// afterwards stays as they left it. The core does not tell values the user
/// cleared from values never set (setting an empty list unsets them), so
/// that it ran is recorded in the view's metadata instead. A database is
/// left alone altogether: its views are created, and this runs, before its
/// metadata and global pointer values are restored, and those then replace
/// any values set here, except an empty list, which is not restored.
pub fn set_global_pointer(view: &BinaryView, value: u32) {
    if view.file().is_database_backed() || view.query_metadata(GLOBAL_POINTER_SET).is_some() {
        return;
    }
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
    // The core sets the whole list at once, so the existing values (owned by
    // us until freed) are copied into the new list.
    let values = unsafe {
        let mut count = 0;
        let existing = BNGetUserGlobalPointerValues(view.handle, &mut count);
        if existing.is_null() {
            with_global_pointer(&[], entry)
        } else {
            let values = with_global_pointer(std::slice::from_raw_parts(existing, count), entry);
            BNFreeRegisterValueWithConfidenceAndRegisterList(existing);
            values
        }
    };
    if let Some(values) = values {
        unsafe { BNSetUserGlobalPointerValues(view.handle, values.as_ptr(), values.len()) };
    }
    view.store_metadata(GLOBAL_POINTER_SET, true, MetadataStoreFlags::PERSISTENT);
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

/// The `ilp32` calling convention: the RISC-V integer ABI, which ESP-IDF
/// uses for the RV32IMC/RV32IMAC chips (they have no FPU).
///
/// Binary Ninja's `rv32gc` `default` convention is the hard-float one: it
/// passes floating point arguments and results in `fa0..fa7`. Under `ilp32`
/// every argument goes in the integer registers `a0..a7`, one register per
/// 4 bytes (see [`ilp32_locations`]); results are in `a0`/`a1`. `s0..s11`
/// are callee-saved and `gp` is the global pointer, as in `default`.
pub struct Ilp32 {
    core: CoreCallingConvention,
    regs: Ilp32Regs,
}

#[derive(Clone)]
struct Ilp32Regs {
    args: Vec<RegisterId>,
    caller_saved: Vec<RegisterId>,
    callee_saved: Vec<RegisterId>,
    gp: RegisterId,
}

impl AsRef<CoreCallingConvention> for Ilp32 {
    fn as_ref(&self) -> &CoreCallingConvention {
        &self.core
    }
}

impl CallingConvention for Ilp32 {
    fn caller_saved_registers(&self) -> Vec<RegisterId> {
        self.regs.caller_saved.clone()
    }

    fn callee_saved_registers(&self) -> Vec<RegisterId> {
        self.regs.callee_saved.clone()
    }

    fn int_arg_registers(&self) -> Vec<RegisterId> {
        self.regs.args.clone()
    }

    fn float_arg_registers(&self) -> Vec<RegisterId> {
        Vec::new()
    }

    fn arg_registers_shared_index(&self) -> bool {
        false
    }

    fn reserved_stack_space_for_arg_registers(&self) -> bool {
        false
    }

    fn stack_adjusted_on_return(&self) -> bool {
        false
    }

    fn is_eligible_for_heuristics(&self) -> bool {
        true
    }

    fn return_int_reg(&self) -> Option<RegisterId> {
        self.regs.args.first().copied()
    }

    fn return_hi_int_reg(&self) -> Option<RegisterId> {
        self.regs.args.get(1).copied()
    }

    fn return_float_reg(&self) -> Option<RegisterId> {
        None
    }

    fn global_pointer_reg(&self) -> Option<RegisterId> {
        Some(self.regs.gp)
    }

    fn implicitly_defined_registers(&self) -> Vec<RegisterId> {
        Vec::new()
    }

    fn are_argument_registers_used_for_var_args(&self) -> bool {
        true
    }

    /// Anything up to two registers wide (floats, doubles, 64-bit integers,
    /// small structures) is passed in registers.
    fn is_argument_type_register_compatible(&self, _view: Option<&BinaryView>, ty: &Type) -> bool {
        ty.width() <= 2 * WORD as u64
    }

    fn parameter_locations(
        &self,
        _view: Option<&BinaryView>,
        _return_value: Option<&ValueLocation>,
        params: &[FunctionParameter],
        permitted_registers: Option<&[RegisterId]>,
    ) -> Vec<ValueLocation> {
        // Registers the caller does not set (when the core asks for the
        // registers a call site actually uses) end the register arguments.
        let regs: Vec<RegisterId> = self
            .regs
            .args
            .iter()
            .copied()
            .take_while(|r| permitted_registers.is_none_or(|p| p.contains(r)))
            .collect();
        let args: Vec<Ilp32Arg> = params
            .iter()
            .map(|p| match &p.location {
                ValueLocationSource::Custom(loc) => {
                    let mut regs_used = Vec::new();
                    let mut stack = Vec::new();
                    for c in &loc.components {
                        match c.variable.ty {
                            VariableSourceType::RegisterVariableSourceType => regs_used
                                .extend(regs.iter().position(|r| r.0 as i64 == c.variable.storage)),
                            VariableSourceType::StackVariableSourceType => {
                                stack.push((c.variable.storage, c.size))
                            }
                            _ => {}
                        }
                    }
                    Ilp32Arg::Fixed {
                        loc: loc.clone(),
                        regs_used,
                        stack_end: fixed_stack_end(&stack, loc.indirect, p.ty.contents.width()),
                    }
                }
                source => Ilp32Arg::Value {
                    size: p.ty.contents.width(),
                    align: p.ty.contents.alignment() as u64,
                    by_reference: *source == ValueLocationSource::PassByReference,
                    by_value: *source == ValueLocationSource::PassByValue,
                },
            })
            .collect();
        ilp32_locations(&args, regs.len())
            .into_iter()
            .map(|loc| match loc {
                Ilp32Loc::Fixed(loc) => loc,
                Ilp32Loc::Parts { parts, indirect } => {
                    let whole = parts.len() == 1;
                    let components = parts
                        .into_iter()
                        .enumerate()
                        .map(|(i, part)| ValueLocationComponent {
                            variable: match part {
                                Ilp32Part::Reg(n) => Variable::from_register_id(regs[n]),
                                Ilp32Part::Stack(off) => Variable::from_stack_offset(off as i64),
                            },
                            offset: (i as u64 * WORD as u64) as i64,
                            size: (!whole).then_some(WORD as u64),
                        })
                        .collect();
                    ValueLocation {
                        components,
                        indirect,
                        returned_pointer: None,
                    }
                }
            })
            .collect()
    }
}

/// The register width.
const WORD: u32 = 4;

/// An argument, for [`ilp32_locations`].
#[derive(Debug, Clone, PartialEq)]
enum Ilp32Arg {
    /// A location the user gave the parameter. It is kept, and the
    /// arguments after it skip the argument registers it uses (by index)
    /// and are placed on the stack after its end. Binary Ninja's own
    /// conventions do this too, except that they reuse a fixed register
    /// that is not the next one.
    Fixed {
        loc: ValueLocation,
        regs_used: Vec<usize>,
        stack_end: u64,
    },
    Value {
        size: u64,
        align: u64,
        /// Passed by reference whatever its size.
        by_reference: bool,
        /// Passed by value whatever its size.
        by_value: bool,
    },
}

/// The end of the stack a fixed location's stack components (offset and
/// size, if given) occupy, rounded up to a word: the stack arguments after
/// it start there. An unsized component holds the whole value, `width`
/// bytes, or for an indirect location only the pointer to it.
fn fixed_stack_end(components: &[(i64, Option<u64>)], indirect: bool, width: u64) -> u64 {
    let word = WORD as u64;
    components
        .iter()
        .map(|&(offset, size)| {
            let size = size.unwrap_or(if indirect { word } else { width });
            (offset.max(0) as u64 + size.max(1)).next_multiple_of(word)
        })
        .max()
        .unwrap_or(0)
}

/// Where an argument goes.
#[derive(Debug, Clone, PartialEq)]
enum Ilp32Loc {
    Fixed(ValueLocation),
    /// A word per part, from the low address up; one part for a value in a
    /// single register or (whole) on the stack.
    Parts {
        parts: Vec<Ilp32Part>,
        indirect: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ilp32Part {
    /// The `n`th argument register.
    Reg(usize),
    /// The stack, at this offset from the stack pointer at the call.
    Stack(u64),
}

/// The ilp32 argument rules of the RISC-V psABI (as Espressif's GCC
/// implements them), with `nregs` argument registers available:
/// - values of up to 4 bytes, `float` included, take the next register;
/// - values of 5 to 8 bytes (`double`, 64-bit integers, small structures)
///   take the next two registers, with no even-register alignment; if only
///   one is left, the low word goes in it and the high word on the stack;
/// - larger values are passed by reference: a pointer, as above;
/// - once the registers run out, arguments go on the stack in order, each
///   aligned to the larger of 4 bytes and its own alignment (so a `double`
///   is 8-aligned), whole.
///
/// Arguments with a fixed location take theirs; the registers it uses are
/// skipped by the arguments after it, which go on the stack after its end.
fn ilp32_locations(args: &[Ilp32Arg], nregs: usize) -> Vec<Ilp32Loc> {
    let word = WORD as u64;
    let mut used = vec![false; nregs];
    let mut next_reg = 0;
    let mut stack = 0u64;
    // The next argument register no fixed location uses, from `next_reg`.
    let take = |used: &mut [bool], next_reg: &mut usize| {
        while *next_reg < nregs && used[*next_reg] {
            *next_reg += 1;
        }
        (*next_reg < nregs).then(|| {
            used[*next_reg] = true;
            *next_reg += 1;
            *next_reg - 1
        })
    };
    args.iter()
        .map(|arg| {
            let Ilp32Arg::Value {
                size,
                align,
                by_reference,
                by_value,
            } = *arg
            else {
                let Ilp32Arg::Fixed {
                    loc,
                    regs_used,
                    stack_end,
                } = arg
                else {
                    unreachable!()
                };
                for &r in regs_used {
                    used[r] = true;
                }
                stack = stack.max(*stack_end);
                return Ilp32Loc::Fixed(loc.clone());
            };
            let indirect = by_reference || (!by_value && size > 2 * word);
            let (size, align) = if indirect {
                (word, word)
            } else {
                (size.max(1), align)
            };
            let words = size.div_ceil(word) as usize;
            if words <= 2
                && let Some(first) = take(&mut used, &mut next_reg)
            {
                let mut parts = vec![Ilp32Part::Reg(first)];
                if words == 2 {
                    if let Some(second) = take(&mut used, &mut next_reg) {
                        parts.push(Ilp32Part::Reg(second));
                    } else {
                        stack = stack.next_multiple_of(word);
                        parts.push(Ilp32Part::Stack(stack));
                        stack += word;
                    }
                }
                return Ilp32Loc::Parts { parts, indirect };
            }
            // On the stack from here on (an argument that did not fit is
            // never followed by one in a register).
            next_reg = nregs;
            let align = align.clamp(word, 2 * word);
            stack = stack.next_multiple_of(align);
            let at = stack;
            stack += (words as u64) * word;
            Ilp32Loc::Parts {
                parts: vec![Ilp32Part::Stack(at)],
                indirect,
            }
        })
        .collect()
}

/// Registers the [`Ilp32`] calling convention on Binary Ninja's `rv32gc`
/// architecture, if it is loaded.
pub fn register() {
    let Some(arch) = CoreArchitecture::by_name(ARCH_NAME) else {
        tracing::info!("architecture {ARCH_NAME} is not available; RISC-V chips are not supported");
        return;
    };
    if arch.calling_convention_by_name(ILP32).is_some() {
        return;
    }
    let reg = |name: String| arch.register_by_name(&name).map(|r| r.id());
    let names = |prefix: &str, lo: u8, hi: u8| {
        (lo..=hi)
            .map(|n| reg(format!("{prefix}{n}")))
            .collect::<Option<Vec<_>>>()
    };
    let (Some(args), Some(t0), Some(t3), Some(callee_saved), Some(ra), Some(gp)) = (
        names("a", 0, 7),
        names("t", 0, 2),
        names("t", 3, 6),
        names("s", 0, 11),
        reg("ra".into()),
        reg("gp".into()),
    ) else {
        tracing::error!("{ARCH_NAME} lacks the expected registers; {ILP32} is not registered");
        return;
    };
    let caller_saved = [vec![ra], t0, args.clone(), t3].concat();
    let regs = Ilp32Regs {
        args,
        caller_saved,
        callee_saved,
        gp,
    };
    register_calling_convention(&arch, ILP32, |core| Ilp32 { core, regs });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(size: u64, align: u64) -> Ilp32Arg {
        Ilp32Arg::Value {
            size,
            align,
            by_reference: false,
            by_value: false,
        }
    }

    fn parts(parts: &[Ilp32Part]) -> Ilp32Loc {
        Ilp32Loc::Parts {
            parts: parts.to_vec(),
            indirect: false,
        }
    }

    /// The cases checked against Espressif GCC 15.2 (`-march=rv32imc
    /// -mabi=ilp32`).
    #[test]
    fn ilp32_argument_locations() {
        use Ilp32Part::{Reg, Stack};
        let int = value(4, 4);
        let float = value(4, 4);
        let double = value(8, 8);
        // f(float), f(double), f(int, double), f(int, float, int)
        assert_eq!(
            ilp32_locations(std::slice::from_ref(&float), 8),
            [parts(&[Reg(0)])]
        );
        assert_eq!(
            ilp32_locations(std::slice::from_ref(&double), 8),
            [parts(&[Reg(0), Reg(1)])]
        );
        assert_eq!(
            ilp32_locations(&[int.clone(), double.clone()], 8),
            [parts(&[Reg(0)]), parts(&[Reg(1), Reg(2)])]
        );
        assert_eq!(
            ilp32_locations(&[int.clone(), float, int.clone()], 8),
            [parts(&[Reg(0)]), parts(&[Reg(1)]), parts(&[Reg(2)])]
        );
        // Seven ints and a double: the double is split between a7 and the
        // stack.
        let mut args = vec![int.clone(); 7];
        args.push(double.clone());
        assert_eq!(ilp32_locations(&args, 8)[7], parts(&[Reg(7), Stack(0)]));
        // Five doubles: the fifth on the stack, whole.
        let locs = ilp32_locations(&vec![double.clone(); 5], 8);
        assert_eq!(locs[3], parts(&[Reg(6), Reg(7)]));
        assert_eq!(locs[4], parts(&[Stack(0)]));
        // Nine ints and a double: the double is 8-aligned on the stack.
        let mut args = vec![int.clone(); 9];
        args.push(double);
        let locs = ilp32_locations(&args, 8);
        assert_eq!(locs[8], parts(&[Stack(0)]));
        assert_eq!(locs[9], parts(&[Stack(8)]));
        // An 8-byte structure in a pair; a 12-byte one by reference.
        assert_eq!(
            ilp32_locations(&[value(8, 4)], 8),
            [parts(&[Reg(0), Reg(1)])]
        );
        assert_eq!(
            ilp32_locations(&[value(12, 4), int.clone()], 8),
            [
                Ilp32Loc::Parts {
                    parts: vec![Reg(0)],
                    indirect: true
                },
                parts(&[Reg(1)])
            ]
        );
        // Fewer registers available (the call site only sets a0, a1).
        assert_eq!(
            ilp32_locations(&[int.clone(), int.clone(), int], 2),
            [parts(&[Reg(0)]), parts(&[Reg(1)]), parts(&[Stack(0)])]
        );
    }

    fn fixed(regs_used: &[usize], stack_end: u64) -> Ilp32Arg {
        Ilp32Arg::Fixed {
            loc: ValueLocation {
                components: vec![],
                indirect: false,
                returned_pointer: None,
            },
            regs_used: regs_used.to_vec(),
            stack_end,
        }
    }

    /// Fixed locations are skipped by the arguments after them. (Binary
    /// Ninja's `default` convention on rv32gc does the same, except that it
    /// reuses a fixed register that is not the next one, overlapping it.)
    #[test]
    fn ilp32_fixed_locations() {
        use Ilp32Part::{Reg, Stack};
        let int = value(4, 4);
        let auto = |locs: Vec<Ilp32Loc>| {
            locs.into_iter()
                .filter(|l| !matches!(l, Ilp32Loc::Fixed(_)))
                .collect::<Vec<_>>()
        };
        // f(a0 fixed, int): the int in a1.
        assert_eq!(
            auto(ilp32_locations(&[fixed(&[0], 0), int.clone()], 8)),
            [parts(&[Reg(1)])]
        );
        // f(a3 fixed, int x4): a0, a1, a2, then a4.
        let mut args = vec![fixed(&[3], 0)];
        args.extend(vec![int.clone(); 4]);
        assert_eq!(
            auto(ilp32_locations(&args, 8)),
            [
                parts(&[Reg(0)]),
                parts(&[Reg(1)]),
                parts(&[Reg(2)]),
                parts(&[Reg(4)])
            ]
        );
        // Only arguments after a fixed one avoid it: f(int, a0 fixed, int).
        assert_eq!(
            auto(ilp32_locations(
                &[int.clone(), fixed(&[0], 0), int.clone()],
                8
            )),
            [parts(&[Reg(0)]), parts(&[Reg(1)])]
        );
        // A double's pair skips a fixed register: f(a1 fixed, double) in a0, a2.
        assert_eq!(
            auto(ilp32_locations(&[fixed(&[1], 0), value(8, 8)], 8)),
            [parts(&[Reg(0), Reg(2)])]
        );
        // A fixed stack slot (or a register outside a0..a7, which is not
        // counted) leaves the registers alone; stack arguments start after
        // the slot's end: f(sp+4 fixed, int x9) has its ninth int at sp+8.
        let mut args = vec![fixed(&[], 8)];
        args.extend(vec![int.clone(); 9]);
        let locs = auto(ilp32_locations(&args, 8));
        assert_eq!(locs[0], parts(&[Reg(0)]));
        assert_eq!(locs[8], parts(&[Stack(8)]));
    }

    #[test]
    fn ilp32_fixed_stack_ends() {
        use Ilp32Part::Stack;
        // An unsized stack component holds the whole value...
        assert_eq!(fixed_stack_end(&[(0, None)], false, 12), 12);
        assert_eq!(fixed_stack_end(&[(4, None)], false, 8), 12);
        // ...or, for an indirect location, the pointer to it.
        assert_eq!(fixed_stack_end(&[(0, None)], true, 12), 4);
        // Explicit sizes are kept, and the highest end of all components
        // counts.
        assert_eq!(fixed_stack_end(&[(0, Some(4)), (4, Some(4))], false, 8), 8);
        assert_eq!(fixed_stack_end(&[], false, 8), 0);
        // The end is rounded up to a word after adding the offset: a 2-byte
        // value at sp+6 ends at sp+8, not sp+10.
        assert_eq!(fixed_stack_end(&[(6, None)], false, 2), 8);
        assert_eq!(fixed_stack_end(&[(5, Some(1))], false, 4), 8);
        // A fixed indirect 12-byte structure (its pointer at sp+0) after
        // eight register arguments: the next int goes to sp+4.
        let int = value(4, 4);
        let mut args = vec![int.clone(); 8];
        args.push(fixed(&[], fixed_stack_end(&[(0, None)], true, 12)));
        args.push(int.clone());
        assert_eq!(ilp32_locations(&args, 8)[9], parts(&[Stack(4)]));
        // A 2-byte fixed parameter at sp+6, seven ints and a double: the
        // double's high word in the next whole word, sp+8.
        let mut args = vec![fixed(&[], fixed_stack_end(&[(6, None)], false, 2))];
        args.extend(vec![int; 7]);
        args.push(value(8, 8));
        assert_eq!(
            ilp32_locations(&args, 8)[8],
            parts(&[Ilp32Part::Reg(7), Stack(8)])
        );
        // Even after an unaligned end the split word stays word-aligned.
        let mut args = vec![fixed(&[], 6)];
        args.extend(vec![value(4, 4); 7]);
        args.push(value(8, 8));
        assert_eq!(
            ilp32_locations(&args, 8)[8],
            parts(&[Ilp32Part::Reg(7), Stack(8)])
        );
    }

    fn pointer_value(reg: u32, value: i64) -> BNRegisterValueWithConfidenceAndRegister {
        BNRegisterValueWithConfidenceAndRegister {
            reg,
            value: BNRegisterValueWithConfidence {
                value: BNRegisterValue {
                    state: BNRegisterValueType::ConstantPointerValue,
                    value,
                    offset: 0,
                    size: 0,
                },
                confidence: 255,
            },
        }
    }

    #[test]
    fn global_pointer_joins_other_user_values() {
        let summary = |vs: &[BNRegisterValueWithConfidenceAndRegister]| {
            vs.iter()
                .map(|v| (v.reg, v.value.value.value))
                .collect::<Vec<_>>()
        };
        let (gp, tp) = (3, 4);
        let new = pointer_value(gp, 0x3FC8_8400);
        // No values: gp is added. (Values the user cleared look the same to
        // the core; set_global_pointer leaves them alone by not running
        // twice on a view.)
        assert_eq!(
            summary(&with_global_pointer(&[], new).unwrap()),
            [(gp, 0x3FC8_8400)]
        );
        // A value for another register: gp is added, the other value kept.
        let values = with_global_pointer(&[pointer_value(tp, 0x1000)], new).unwrap();
        assert_eq!(summary(&values), [(tp, 0x1000), (gp, 0x3FC8_8400)]);
        // A value for gp itself is never replaced.
        assert!(
            with_global_pointer(&[pointer_value(tp, 0x1000), pointer_value(gp, 0x1234)], new)
                .is_none()
        );
    }

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
