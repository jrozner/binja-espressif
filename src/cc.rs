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
    CallingConvention, ConventionBuilder, CoreCallingConvention, register_calling_convention,
};
use binaryninja::confidence::MAX_CONFIDENCE;
use binaryninja::function::Function;
use binaryninja::function_recognizer::FunctionRecognizer;
use binaryninja::low_level_il::LowLevelILRegularFunction;
use binaryninja::rc::Ref;

use crate::arch::Xtensa;
use crate::registers::Reg;

fn a(n: u8) -> RegisterId {
    Reg::A(n).id()
}

fn range(lo: u8, hi: u8) -> Vec<RegisterId> {
    (lo..=hi).map(a).collect()
}

pub struct WindowedCC {
    core: CoreCallingConvention,
}

impl AsRef<CoreCallingConvention> for WindowedCC {
    fn as_ref(&self) -> &CoreCallingConvention {
        &self.core
    }
}

impl CallingConvention for WindowedCC {
    fn caller_saved_registers(&self) -> Vec<RegisterId> {
        range(8, 15)
    }

    fn callee_saved_registers(&self) -> Vec<RegisterId> {
        range(0, 7)
    }

    fn int_arg_registers(&self) -> Vec<RegisterId> {
        range(10, 15)
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
        Some(a(10))
    }

    fn return_hi_int_reg(&self) -> Option<RegisterId> {
        Some(a(11))
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
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

pub struct Conventions {
    pub call0: Ref<CoreCallingConvention>,
    pub windowed: Option<Ref<CoreCallingConvention>>,
}

/// Registers the calling conventions of a core and sets its default.
pub fn register(arch: &'static Xtensa) -> Conventions {
    let names = |lo: u8, hi: u8| (lo..=hi).map(|n| format!("a{n}")).collect::<Vec<_>>();
    // On the windowed cores, call0 functions are recognized by their missing
    // `entry` instruction (see `WindowRecognizer`) rather than heuristically:
    // the heuristics would prefer call0 for every windowed function, since
    // those read their arguments from a2..a7 as well.
    let call0 = ConventionBuilder::new(arch)
        .is_eligible_for_heuristics(!arch.chip.windowed())
        .int_arg_registers(&strs(&names(2, 7)))
        .return_int_reg("a2")
        .return_hi_int_reg("a3")
        .caller_saved_registers(&strs(&[names(0, 0), names(2, 11)].concat()))
        .callee_saved_registers(&strs(&names(12, 15)))
        .are_argument_registers_used_for_var_args(true)
        .register("call0");

    let windowed = arch
        .chip
        .windowed()
        .then(|| register_calling_convention(arch, "windowed", |core| WindowedCC { core }));

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
///   such as exception vectors).
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
        let mut code = [0u8; 3];
        if bv.read(&mut code, func.start()) != 3 {
            return false;
        }
        if crate::esp::is_entry_insn(&code) {
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
        } else if func
            .calling_convention()
            .is_none_or(|cc| cc.contents.name() != "call0")
        {
            func.set_auto_calling_convention(Some(&*self.call0));
        }
        false
    }
}
