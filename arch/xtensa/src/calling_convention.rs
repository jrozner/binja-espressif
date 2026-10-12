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
use binaryninja::confidence::{Conf, MAX_CONFIDENCE};
use binaryninja::function::{Function, FunctionUpdateType};
use binaryninja::function_recognizer::FunctionRecognizer;
use binaryninja::low_level_il::LowLevelILRegularFunction;
use binaryninja::low_level_il::instruction::{InstructionHandler, LowLevelILInstructionKind};
use binaryninja::rc::Ref;
use binaryninja::symbol::SymbolType;
use binaryninja::types::{
    FunctionParameter, Type, ValueLocation, ValueLocationComponent, ValueLocationSource,
};
use binaryninja::variable::{RegisterValueType, Variable, VariableSourceType};
use binaryninja::workflow::{Activity, AnalysisContext, Workflow, activity};

use crate::Core;
use crate::architecture::Xtensa;
use crate::register::Reg;

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

/// Registers the calling conventions of a core and sets its default.
/// Returns the `call0` convention.
pub fn register(arch: &'static Xtensa) -> Ref<CoreCallingConvention> {
    // On the windowed cores, call0 functions are recognized by their missing
    // `entry` instruction (see `CALL0_STEP`) rather than heuristically:
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
    call0
}

/// The analysis step that gives the functions on the windowed cores that
/// do not start with `entry` the `call0` convention: every windowed-ABI
/// function does, so these are hand-written routines called with
/// `call0`/`callx0`, whole programs built with `-mabi=call0`, or not
/// functions in the C sense at all, such as exception vectors.
///
/// This is only a default: a function with a user type is left alone, so a
/// convention the user chose (with `__convention("windowed")` in its type, or
/// by setting the calling convention, which also makes the type a user type)
/// sticks. That matters for code that has no `entry` but is called with
/// `call4`/`call8`/`call12`, such as placeholder stubs for unresolved
/// externals: typed as `call0`, their callers' arguments would be read from
/// `a2..a7` instead of `a10..a15`. Functions whose type was imported from a
/// type library are left alone too (see [`has_imported_type`]).
///
/// It runs in every analysis of a function, right after lifting: a full
/// update resets the convention to the platform's default (`windowed`), and
/// the parameters and return value are inferred from the convention when the
/// MLIL is generated. A function recognizer runs too late for that, after
/// the function's analysis is complete, and the core does not analyze the
/// function again for a convention set there.
const CALL0_STEP: &str = "xtensa.function.call0Convention";
/// The step [`CALL0_STEP`] runs after.
const LIFT_STEP: &str = "core.function.generateLiftedIL";

/// Registers [`CALL0_STEP`].
pub fn register_call0_step() {
    let config = activity::Config::action(
        CALL0_STEP,
        "Xtensa call0 convention",
        "Gives the functions on the windowed Xtensa cores that do not start with entry the call0 calling convention.",
    );
    // The action must not capture anything: `Activity::new_with_action`
    // keeps a pointer to the closure, which it does not keep alive.
    let step = Activity::new_with_action(config, |ctx: &AnalysisContext| {
        let func = ctx.function();
        let arch = func.arch();
        let name = arch.name();
        if Core::ALL
            .iter()
            .any(|core| core.windowed() && core.arch_name() == name)
            && let Some(call0) = arch.calling_convention_by_name("call0")
        {
            default_to_call0(&ctx.view(), &func, &call0);
        }
    });
    let registered = Workflow::cloned("core.function.metaAnalysis")
        .ok_or(())
        .and_then(|w| w.activity_after(&step, LIFT_STEP))
        .and_then(|w| w.register());
    if registered.is_err() {
        tracing::error!("cannot register the Xtensa call0 convention step");
    }
}

fn default_to_call0(bv: &BinaryView, func: &Function, call0: &CoreCallingConvention) {
    if func.has_user_type() || has_imported_type(bv, func) {
        return;
    }
    // A function in the last two bytes of a segment (a `ret.n` stub, say)
    // has no room for `entry`; it is still a call0 function.
    let mut code = [0u8; 3];
    let len = bv.read(&mut code, func.start());
    if len == 0 || crate::is_entry_insn(&code[..len]) {
        return;
    }
    if func
        .calling_convention()
        .is_none_or(|cc| cc.contents.name() != "call0")
    {
        func.set_auto_calling_convention(Some(call0));
    }
}

/// Is `func` part of an external API whose type was imported from a type
/// library (as recorded with `BinaryView::record_imported_object_library`),
/// either defined as an imported function or with that type pinned as a user
/// type? Its type then says how to call it, and whoever imported it says
/// what it clobbers. Espressif's mask ROM functions are: the Espressif view
/// defines them as imported functions typed from the chip's ROM type library
/// (and makes them clobber the convention's caller-saved registers),
/// whether their region holds zeros or a ROM image (whose jump table entries
/// have no `entry` either), and pins the types of the ROM functions of a ROM
/// ELF file opened on its own. Other functions with an imported type keep
/// the fixes that follow from their code.
fn has_imported_type(bv: &BinaryView, func: &Function) -> bool {
    (func.symbol().sym_type() == SymbolType::ImportedFunction || func.has_user_type())
        && bv
            .lookup_imported_object_library(func.start(), &func.platform())
            .is_some()
}

/// Fixes up functions on the windowed cores after their analysis:
///
/// * Windowed functions clobber exactly `a8..a15` of their caller (the
///   registers that become the callee's `a0..a7`). The core would otherwise
///   derive the clobbered set from the function's own writes to `a0..a7`,
///   which belong to the callee's window and never affect the caller.
/// * `callx0` sites are typed as calls with the `call0` convention. The core
///   takes the convention of a call through a register from the platform
///   (`windowed`), whatever the caller's or the callee's: with the target
///   unknown, the arguments would be read from `a10..a15` and the result
///   from `a10`, and `a12..a15` would not survive the call; with a target
///   that resolves to a function, the arguments follow its type, but the
///   result is still looked for in `a10`. A call site's type is the only
///   convention the core takes per call, so these get the callee's type,
///   or, for an unknown target, a variadic `call0` type whose arguments are
///   the argument registers the caller sets. Call sites typed by the user
///   are left alone, and so are calls through a typed function pointer: the
///   core types those from the pointer itself, at full confidence, on every
///   analysis, so the pointer's type needs `__convention("call0")`.
///
/// Functions whose type was imported from a type library (see
/// [`has_imported_type`]) only get the second: their type and what they
/// clobber come from the API they belong to, but the calls they make are
/// theirs.
pub struct WindowRecognizer {
    pub call0: Ref<CoreCallingConvention>,
}

/// The confidence of the `callx0` sites' types: below the user's, but not
/// below Binary Ninja's heuristic confidence (`BN_HEURISTIC_CONFIDENCE`).
/// In a function with a user type (a typed ROM function's, say), a call
/// through a parameter of a pointer type that is not a function pointer
/// (`memset`'s `void *`) loses a call type below that on every analysis:
/// typed again each time, the function would be analyzed again until Binary
/// Ninja skipped its analysis.
const CALL_TYPE_CONFIDENCE: u8 = 192;

impl FunctionRecognizer for WindowRecognizer {
    fn recognize_low_level_il(
        &self,
        bv: &BinaryView,
        func: &Function,
        llil: &LowLevelILRegularFunction,
    ) -> bool {
        // An imported function's own calls are analyzed like any other's.
        self.type_call0_sites(bv, func, llil);
        if has_imported_type(bv, func) {
            return false;
        }
        let mut code = [0u8; 3];
        let len = bv.read(&mut code, func.start());
        if !crate::is_entry_insn(&code[..len]) {
            return false;
        }
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
        false
    }
}

impl WindowRecognizer {
    /// Types `func`'s `callx0` sites (see [`WindowRecognizer`]). The
    /// recognizer runs once the function's analysis is complete, so the
    /// function is analyzed again if any type changed.
    fn type_call0_sites(&self, bv: &BinaryView, func: &Function, llil: &LowLevelILRegularFunction) {
        let platform = func.platform();
        let mut changed = false;
        for block in llil.basic_blocks().iter() {
            for insn in block.iter() {
                let LowLevelILInstructionKind::Call(op) = insn.kind() else {
                    continue;
                };
                let addr = insn.address();
                let mut code = [0u8; 3];
                if bv.read(&mut code, addr) < 3 || !is_callx0(&code) {
                    continue;
                }
                let target = op.target().value();
                let callee = matches!(
                    target.state,
                    RegisterValueType::ConstantValue | RegisterValueType::ConstantPointerValue
                )
                .then(|| bv.function_at(&platform, target.value as u64))
                .flatten();
                let ty = match callee {
                    Some(callee) => callee.function_type(),
                    None => Type::function_with_opts(
                        Type::int(WORD, true),
                        &[],
                        true,
                        Conf::new(self.call0.clone(), MAX_CONFIDENCE),
                        Conf::new(0, MAX_CONFIDENCE),
                    ),
                };
                if let Some(current) = func.call_type_adjustment(addr, None)
                    && (current.confidence == MAX_CONFIDENCE
                        || (current.confidence == CALL_TYPE_CONFIDENCE && current.contents == ty))
                {
                    continue;
                }
                func.set_auto_call_type_adjustment(
                    addr,
                    Conf::new(&*ty, CALL_TYPE_CONFIDENCE),
                    None,
                );
                changed = true;
            }
        }
        if changed {
            // The parameters inferred with the call sites untyped would stay:
            // the core keeps those it inferred with full confidence through
            // later updates.
            func.set_auto_parameter_locations(Vec::new(), 0);
            func.mark_updates_required(FunctionUpdateType::IncrementalAutoFunctionUpdate);
        }
    }
}

/// Is `code` a `callx0`?
fn is_callx0(code: &[u8]) -> bool {
    // callx0 as: byte0 = t << 4 | op0 with t = m << 2 | n = 0b1100,
    // byte1 = r << 4 | s with r = 0, byte2 = op2 << 4 | op1 = 0.
    code.len() >= 3 && code[0] == 0xc0 && code[1] & 0xf0 == 0 && code[2] == 0
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
    fn callx0_encoding() {
        // callx0 a0, callx0 a9 (as objdump shows them: 0000c0, 0009c0).
        assert!(is_callx0(&[0xc0, 0x00, 0x00]));
        assert!(is_callx0(&[0xc0, 0x09, 0x00]));
        // callx8 a9, ret, call0 and a truncated callx0 are not.
        assert!(!is_callx0(&[0xe0, 0x09, 0x00]));
        assert!(!is_callx0(&[0x80, 0x00, 0x00]));
        assert!(!is_callx0(&[0x05, 0xee, 0xff]));
        assert!(!is_callx0(&[0xc0, 0x00]));
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
