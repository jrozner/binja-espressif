//! Binary Ninja plugin adding the `ilp32` calling convention to Binary
//! Ninja's `rv32gc` architecture (the `arch_riscv` plugin): the soft-float
//! RISC-V integer ABI that ESP-IDF uses on Espressif's RISC-V cores, which
//! have no FPU. Nothing here knows about Espressif's chips or firmware (see
//! `view_espressif`). It belongs in `arch_riscv` itself, and is kept here
//! until it is proposed there.

use binaryninja::architecture::{ArchitectureExt, CoreArchitecture, Register, RegisterId};
use binaryninja::binary_view::BinaryView;
use binaryninja::calling_convention::{
    CallingConvention, CoreCallingConvention, register_calling_convention,
};
use binaryninja::types::{
    FunctionParameter, Type, ValueLocation, ValueLocationComponent, ValueLocationSource,
};
use binaryninja::variable::{Variable, VariableSourceType};

/// The name of Binary Ninja's RISC-V architecture the convention is for.
pub const ARCH_NAME: &str = "rv32gc";

/// The soft-float calling convention (see [`register`]).
pub const ILP32: &str = "ilp32";

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

/// The plugin's entry point when it is built on its own (without the
/// `bundled` feature, which leaves it to the plugin that links this crate).
#[cfg(not(feature = "bundled"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginInit() -> bool {
    binaryninja::tracing_init!("RISC-V");
    register();
    true
}

/// See [`CorePluginInit`].
#[cfg(not(feature = "bundled"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn CorePluginDependencies() {
    plugin_dependencies();
}

/// Declares the plugins this one needs loaded first: Binary Ninja's RISC-V
/// architecture plugin, whose `rv32gc` gets the convention.
pub fn plugin_dependencies() {
    binaryninja::add_optional_plugin_dependency("arch_riscv");
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
}
