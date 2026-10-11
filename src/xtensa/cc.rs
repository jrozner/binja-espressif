//! Calling conventions.
//!
//! * `call0`: the non-windowed ABI, used by the ESP8266 and by hand-written
//!   code on the other chips. Arguments in `a2..a7`, result in `a2`/`a3`,
//!   return address in `a0`, `a12..a15` callee-saved.
//! * `windowed`: the windowed ABI that GCC uses for everything on the ESP32
//!   family, where every call is a `call8`/`callx8`. The register window
//!   rotates by eight on the call: the caller passes arguments in `a10..a15`
//!   and receives the result in `a10`/`a11`, while the callee sees the same
//!   physical registers as `a2..a7` and returns in `a2`/`a3`.
//!
//!   Binary Ninja has no register windows, so the convention is described
//!   from the caller's side, and the callee is lifted to match: `entry`
//!   copies `a10..a15` to `a2..a7` and `retw` copies `a2`/`a3` to
//!   `a10`/`a11`, much like a SPARC `save`/`restore`. Parameters are then
//!   inferred in the convention's registers and call sites need no
//!   translation. `call4`/`call12` sites are lifted with explicit moves into
//!   and out of the `call8` registers. (The calling convention API's
//!   parameter/incoming variable mapping, meant for this, is not applied to
//!   register parameters by the core; see LIMITATIONS.md.)
//!
//! Floating point arguments and results are passed in address registers in
//! both ABIs.

use binaryninja::architecture::{
    Architecture, ArchitectureExt, CoreRegister, Register as _, RegisterId,
};
use binaryninja::binary_view::{BinaryView, BinaryViewBase};
use binaryninja::calling_convention::{
    CallingConvention, CoreCallingConvention, register_calling_convention,
};
use binaryninja::confidence::MAX_CONFIDENCE;
use binaryninja::function::Function;
use binaryninja::function_recognizer::FunctionRecognizer;
use binaryninja::low_level_il::LowLevelILRegularFunction;
use binaryninja::rc::Ref;
use binaryninja::types::{
    FunctionParameter, Type, ValueLocation, ValueLocationComponent, ValueLocationSource,
};
use binaryninja::variable::{Variable, VariableSourceType};

use crate::xtensa::arch::Xtensa;
use crate::xtensa::registers::Reg;

fn a(n: u8) -> RegisterId {
    Reg::A(n).id()
}

fn range(lo: u8, hi: u8) -> Vec<RegisterId> {
    (lo..=hi).map(a).collect()
}

/// The registers of one of the two conventions.
#[derive(Clone)]
struct Regs {
    /// The six argument registers, as the caller sees them.
    args: Vec<RegisterId>,
    caller_saved: Vec<RegisterId>,
    callee_saved: Vec<RegisterId>,
    heuristics: bool,
}

impl Regs {
    fn call0(heuristics: bool) -> Regs {
        Regs {
            args: range(2, 7),
            caller_saved: [vec![a(0)], range(2, 11)].concat(),
            callee_saved: range(12, 15),
            heuristics,
        }
    }

    fn windowed() -> Regs {
        Regs {
            args: range(10, 15),
            caller_saved: range(8, 15),
            callee_saved: range(0, 7),
            heuristics: true,
        }
    }
}

/// The `call0` or `windowed` calling convention (see the module
/// documentation), with the Xtensa argument rules of [`xtensa_locations`].
pub struct XtensaCC {
    core: CoreCallingConvention,
    regs: Regs,
}

impl AsRef<CoreCallingConvention> for XtensaCC {
    fn as_ref(&self) -> &CoreCallingConvention {
        &self.core
    }
}

impl CallingConvention for XtensaCC {
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
        self.regs.heuristics
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
        None
    }

    fn implicitly_defined_registers(&self) -> Vec<RegisterId> {
        Vec::new()
    }

    fn are_argument_registers_used_for_var_args(&self) -> bool {
        true
    }

    /// Anything that fits in the six argument registers can be passed in
    /// registers (see [`xtensa_locations`]).
    fn is_argument_type_register_compatible(&self, _view: Option<&BinaryView>, ty: &Type) -> bool {
        ty.width() <= (ARG_REGS * WORD) as u64
    }

    fn parameter_locations(
        &self,
        _view: Option<&BinaryView>,
        _return_value: Option<&ValueLocation>,
        params: &[FunctionParameter],
        permitted_registers: Option<&[RegisterId]>,
    ) -> Vec<ValueLocation> {
        let regs = &self.regs.args;
        // When the core asks for the registers a call site actually sets,
        // an argument that needs one it does not set ends the register
        // arguments. A register an 8-byte argument skips to start at an even
        // one need not be set.
        let permitted: Vec<bool> = regs
            .iter()
            .map(|r| permitted_registers.is_none_or(|p| p.contains(r)))
            .collect();
        // A parameter passed by reference is a pointer to the value.
        let indirect: Vec<bool> = params
            .iter()
            .map(|p| p.location == ValueLocationSource::PassByReference)
            .collect();
        let args: Vec<XtensaArg> = params
            .iter()
            .map(|p| match &p.location {
                ValueLocationSource::Custom(loc) => {
                    let mut regs_used = Vec::new();
                    let mut stack_end = 0;
                    for c in &loc.components {
                        match c.variable.ty {
                            VariableSourceType::RegisterVariableSourceType => regs_used
                                .extend(regs.iter().position(|r| r.0 as i64 == c.variable.storage)),
                            VariableSourceType::StackVariableSourceType => {
                                let size = c.size.unwrap_or(if loc.indirect {
                                    WORD as u64
                                } else {
                                    p.ty.contents.width()
                                });
                                let end = c.variable.storage.max(0) as u64 + size.max(1);
                                stack_end = stack_end.max(end.next_multiple_of(WORD as u64));
                            }
                            _ => {}
                        }
                    }
                    XtensaArg::Fixed {
                        loc: loc.clone(),
                        regs_used,
                        stack_end,
                    }
                }
                ValueLocationSource::PassByReference => XtensaArg::Value {
                    size: WORD as u64,
                    align: WORD as u64,
                },
                // Xtensa passes everything else by value anyway.
                _ => XtensaArg::Value {
                    size: p.ty.contents.width(),
                    align: p.ty.contents.alignment() as u64,
                },
            })
            .collect();
        xtensa_locations(&args, &permitted)
            .into_iter()
            .zip(indirect)
            .map(|(loc, indirect)| match loc {
                XtensaLoc::Fixed(loc) => loc,
                XtensaLoc::Regs(first, count) => ValueLocation {
                    components: (0..count)
                        .map(|i| ValueLocationComponent {
                            variable: Variable::from_register_id(regs[first + i]),
                            offset: (i * WORD) as i64,
                            size: (count > 1).then_some(WORD as u64),
                        })
                        .collect(),
                    indirect,
                    returned_pointer: None,
                },
                XtensaLoc::Stack(offset) => ValueLocation {
                    components: vec![ValueLocationComponent {
                        variable: Variable::from_stack_offset(offset as i64),
                        offset: 0,
                        size: None,
                    }],
                    indirect,
                    returned_pointer: None,
                },
            })
            .collect()
    }
}

/// The register width.
const WORD: usize = 4;
/// The number of argument registers.
const ARG_REGS: usize = 6;

/// An argument, for [`xtensa_locations`].
#[derive(Debug, Clone, PartialEq)]
enum XtensaArg {
    /// A location the user gave the parameter. It is kept; the arguments
    /// after it skip the argument registers it uses (by index), and those
    /// on the stack go after its end.
    Fixed {
        loc: ValueLocation,
        regs_used: Vec<usize>,
        stack_end: u64,
    },
    Value {
        size: u64,
        align: u64,
    },
}

/// Where an argument goes.
#[derive(Debug, Clone, PartialEq)]
enum XtensaLoc {
    Fixed(ValueLocation),
    /// `count` consecutive argument registers from the `first`th, the low
    /// word in the first.
    Regs(usize, usize),
    /// The stack, at this offset from the stack pointer at the call.
    Stack(u64),
}

/// The Xtensa argument rules (GCC's `xtensa_function_arg`), with the
/// argument registers whose `permitted` entry is set available:
/// - every argument, aggregates included, takes as many consecutive
///   registers as it has words, the next ones free;
/// - one aligned to more than a word (`double`, 64-bit integers, structures
///   holding those) starts at an even register;
/// - one that does not fit in the registers left goes on the stack, whole,
///   and so do all the arguments after it, each aligned to the larger of a
///   word and its own alignment (at most 16 bytes); so does one that would
///   take a register that is not available (the register an argument
///   skips to start at an even one need not be).
fn xtensa_locations(args: &[XtensaArg], permitted: &[bool]) -> Vec<XtensaLoc> {
    let word = WORD as u64;
    let nregs = permitted.len();
    let mut used = vec![false; nregs];
    let mut next = 0usize;
    let mut stack = 0u64;
    args.iter()
        .map(|arg| {
            let &XtensaArg::Value { size, align } = arg else {
                let XtensaArg::Fixed {
                    loc,
                    regs_used,
                    stack_end,
                } = arg
                else {
                    unreachable!()
                };
                for &r in regs_used {
                    if let Some(u) = used.get_mut(r) {
                        *u = true;
                    }
                }
                stack = stack.max(*stack_end);
                return XtensaLoc::Fixed(loc.clone());
            };
            let words = size.max(1).div_ceil(word) as usize;
            let step = if align > word { 2 } else { 1 };
            let mut first = next.next_multiple_of(step);
            while first + words <= nregs && used[first..first + words].iter().any(|&u| u) {
                first += step;
            }
            if first + words <= nregs && permitted[first..first + words].iter().all(|&p| p) {
                used[first..first + words].fill(true);
                next = first + words;
                return XtensaLoc::Regs(first, words);
            }
            next = nregs;
            stack = stack.next_multiple_of(align.clamp(word, 4 * word));
            let at = stack;
            stack += words as u64 * word;
            XtensaLoc::Stack(at)
        })
        .collect()
}

pub struct Conventions {
    pub call0: Ref<CoreCallingConvention>,
    pub windowed: Option<Ref<CoreCallingConvention>>,
}

/// Registers the calling conventions of a core and sets its default.
pub fn register(arch: &'static Xtensa) -> Conventions {
    // On the windowed cores, call0 functions are recognized by their missing
    // `entry` instruction (see `WindowRecognizer`) rather than heuristically:
    // the heuristics would prefer call0 for every windowed function, since
    // those read their arguments from a2..a7 as well.
    let call0_regs = Regs::call0(!arch.core.windowed());
    let call0 = register_calling_convention(arch, "call0", |core| XtensaCC {
        core,
        regs: call0_regs,
    });

    let windowed = arch.core.windowed().then(|| {
        register_calling_convention(arch, "windowed", |core| XtensaCC {
            core,
            regs: Regs::windowed(),
        })
    });

    let default = windowed.as_ref().unwrap_or(&call0);
    arch.set_default_calling_convention(default);
    if let Some(platform) = arch.standalone_platform() {
        platform.set_default_calling_convention(default);
    }
    Conventions { call0, windowed }
}

/// Fixes up functions on the windowed cores after lifting:
///
/// * Functions that do not start with `entry` get the `call0` convention:
///   every windowed-ABI function does, so these are hand-written routines
///   called with `call0`/`callx0` (or not functions in the C sense at all,
///   such as exception vectors). This is only a default: a function with a
///   user type is left alone, so a convention the user chose (with
///   `__convention("windowed")` in its type, or by setting the calling
///   convention, which also makes the type a user type) sticks. That matters
///   for code that has no `entry` but is called with `call4`/`call8`/`call12`,
///   such as placeholder stubs for unresolved externals: typed as `call0`,
///   their callers' arguments would be read from `a2..a7` instead of
///   `a10..a15`.
///   The typed mask ROM functions in the plugin's ROM region are left alone
///   (see [`crate::espressif::rom_types`]); the other functions there, in
///   a ROM image, are code like any other.
/// * Windowed functions clobber exactly `a8..a15` of their caller (the
///   registers that become the callee's `a0..a7`). The core would otherwise
///   derive the clobbered set from the function's own writes to `a0..a7`,
///   which belong to the callee's window and never affect the caller.
pub struct WindowRecognizer {
    pub call0: Ref<CoreCallingConvention>,
}

impl FunctionRecognizer for WindowRecognizer {
    fn recognize_low_level_il(
        &self,
        bv: &BinaryView,
        func: &Function,
        _llil: &LowLevelILRegularFunction,
    ) -> bool {
        // Mask ROM functions are typed from the ROM type library: their
        // type says how to call them. Their region holds zeros, or a ROM
        // image whose jump table entries have no `entry` either.
        if crate::espressif::rom_types::rom_function_name(bv, func).is_some() {
            return false;
        }
        // A function in the last two bytes of a segment (a `ret.n` stub,
        // say) has no room for `entry`; it is still a call0 function.
        let mut code = [0u8; 3];
        let len = bv.read(&mut code, func.start());
        if len == 0 {
            return false;
        }
        if crate::xtensa::is_entry_insn(&code[..len]) {
            let arch = func.arch();
            let clobbered: Vec<CoreRegister> = (8..=15)
                .filter_map(|n| arch.register_from_id(RegisterId(n)))
                .collect();
            let current = func.clobbered_registers();
            let mut ids: Vec<u32> = current.contents.iter().map(|r| r.id().0).collect();
            ids.sort();
            if current.confidence < MAX_CONFIDENCE || ids != (8..=15).collect::<Vec<u32>>() {
                func.set_auto_clobbered_registers(clobbered, MAX_CONFIDENCE);
            }
        } else if !func.has_user_type()
            && func
                .calling_convention()
                .is_none_or(|cc| cc.contents.name() != "call0")
        {
            func.set_auto_calling_convention(Some(&*self.call0));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every argument register available.
    const ALL: [bool; ARG_REGS] = [true; ARG_REGS];

    fn value(size: u64, align: u64) -> XtensaArg {
        XtensaArg::Value { size, align }
    }

    /// The cases checked against Espressif GCC 15.2 (`xtensa-esp-elf`,
    /// ESP32), as register indices from the first argument register.
    #[test]
    fn xtensa_argument_locations() {
        use XtensaLoc::{Regs, Stack};
        let int = value(4, 4);
        let long_long = value(8, 8);
        let triple = value(12, 4);
        // f(int, long long, int)
        assert_eq!(
            xtensa_locations(&[int.clone(), long_long.clone(), int.clone()], &ALL),
            [Regs(0, 1), Regs(2, 2), Regs(4, 1)]
        );
        // f(5 ints, double, int): the double does not fit in the last
        // register, so it and everything after it go on the stack.
        let mut args = vec![int.clone(); 5];
        args.extend([long_long.clone(), int.clone()]);
        let got = xtensa_locations(&args, &ALL);
        assert_eq!(got[4..], [Regs(4, 1), Stack(0), Stack(8)]);
        // f(struct {int a, b, c;}), f(int, that struct, int, int)
        assert_eq!(
            xtensa_locations(std::slice::from_ref(&triple), &ALL),
            [Regs(0, 3)]
        );
        assert_eq!(
            xtensa_locations(&[int.clone(), triple, int.clone(), int.clone()], &ALL),
            [Regs(0, 1), Regs(1, 3), Regs(4, 1), Regs(5, 1)]
        );
        // f(struct {int a[8];}, int): too big for the registers.
        assert_eq!(
            xtensa_locations(&[value(32, 4), int.clone()], &ALL),
            [Stack(0), Stack(32)]
        );
        // f(7 ints, double): the double is 8-aligned on the stack.
        let mut args = vec![int.clone(); 7];
        args.push(long_long);
        let got = xtensa_locations(&args, &ALL);
        assert_eq!(got[5..], [Regs(5, 1), Stack(0), Stack(8)]);
    }

    /// The registers a call site does not set.
    #[test]
    fn arguments_stop_at_unset_registers() {
        use XtensaLoc::{Regs, Stack};
        let int = value(4, 4);
        // f(int, long long) at a site setting the first, third and fourth:
        // the second is the long long's alignment gap.
        let permitted = [true, false, true, true, false, false];
        assert_eq!(
            xtensa_locations(&[int.clone(), value(8, 8)], &permitted),
            [Regs(0, 1), Regs(2, 2)]
        );
        // f(int, int, int) at the same site: the second int would need the
        // unset second register, so it and the third go on the stack.
        assert_eq!(
            xtensa_locations(&[int.clone(), int.clone(), int.clone()], &permitted),
            [Regs(0, 1), Stack(0), Stack(4)]
        );
    }

    #[test]
    fn arguments_skip_fixed_registers() {
        use XtensaLoc::{Regs, Stack};
        let loc = ValueLocation {
            components: Vec::new(),
            indirect: false,
            returned_pointer: None,
        };
        let fixed = XtensaArg::Fixed {
            loc: loc.clone(),
            regs_used: vec![0],
            stack_end: 8,
        };
        let got = xtensa_locations(&[fixed, value(4, 4), value(8, 8)], &ALL);
        assert_eq!(got, [XtensaLoc::Fixed(loc), Regs(1, 1), Regs(2, 2)]);
        // With the registers taken up to the last, a pair goes on the stack
        // after the fixed slot's end.
        let fixed = XtensaArg::Fixed {
            loc: ValueLocation {
                components: Vec::new(),
                indirect: false,
                returned_pointer: None,
            },
            regs_used: vec![4],
            stack_end: 8,
        };
        let mut args = vec![fixed];
        args.extend(vec![value(4, 4); 4]);
        args.push(value(8, 8));
        assert_eq!(xtensa_locations(&args, &ALL)[5], Stack(8));
    }
}
