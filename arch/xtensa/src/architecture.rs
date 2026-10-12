//! The Binary Ninja architecture.

use std::collections::HashMap;

use binaryninja::Endianness;
use binaryninja::architecture::{
    Architecture, BranchKind, CoreArchitecture, CustomArchitectureHandle, FlagClassId, FlagGroupId,
    FlagId, FlagWriteId, InstructionInfo, IntrinsicId, LinearSweepAnalysisCapability, RegisterId,
    UnusedRegisterStack,
};
use binaryninja::disassembly::InstructionTextToken;
use binaryninja::low_level_il::LowLevelILMutableFunction;
use xtensa_isa::{ControlFlow, Features, Instruction, Isa};

use crate::flag::{Flag, FlagClass, FlagGroup, FlagWrite};
use crate::intrinsic::{self, Intrinsic};
use crate::lift::Lifter;
use crate::loops::Loops;
use crate::patch;
use crate::register::Reg;
use crate::{Core, token};

pub struct Xtensa {
    handle: CoreArchitecture,
    custom_handle: CustomArchitectureHandle<Xtensa>,
    pub core: Core,
    isa: Isa,
    intrinsics: HashMap<u32, Intrinsic>,
    registers: Vec<Reg>,
    /// Zero-overhead loops seen so far; `None` without the loop option.
    loops: Option<Loops>,
}

impl Xtensa {
    pub fn new(
        core: Core,
        handle: CoreArchitecture,
        custom_handle: CustomArchitectureHandle<Xtensa>,
    ) -> Self {
        let isa = core.isa();
        Xtensa {
            handle,
            custom_handle,
            core,
            isa,
            intrinsics: intrinsic::build(&isa),
            registers: Reg::all(&isa),
            loops: isa.has(Features::LOOP).then(Loops::default),
        }
    }

    fn decode(&self, data: &[u8], addr: u64) -> Option<Instruction> {
        self.isa.decode(data, addr as u32).ok()
    }

    fn loop_back(&self, addr: u64, next: u64) -> Option<u64> {
        self.loops.as_ref()?.loop_back(addr, next)
    }
}

/// Runs an architecture callback, turning a panic into `None`: a bug must
/// not take the whole process down (a panic cannot unwind into the core).
pub(crate) fn guard<T>(what: &str, addr: u64, f: impl FnOnce() -> Option<T>) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        tracing::error!("{what} panicked at {addr:#x}");
        None
    })
}

/// [`guard`] for the patching callbacks, where a panic means "not patched".
fn patching(what: &str, addr: u64, f: impl FnOnce() -> bool) -> bool {
    guard(what, addr, || Some(f())).unwrap_or(false)
}

impl AsRef<CoreArchitecture> for Xtensa {
    fn as_ref(&self) -> &CoreArchitecture {
        &self.handle
    }
}

impl Architecture for Xtensa {
    type Handle = CustomArchitectureHandle<Self>;
    type RegisterInfo = Reg;
    type Register = Reg;
    type RegisterStackInfo = UnusedRegisterStack<Reg>;
    type RegisterStack = UnusedRegisterStack<Reg>;
    type Flag = Flag;
    type FlagWrite = FlagWrite;
    type FlagClass = FlagClass;
    type FlagGroup = FlagGroup;
    type Intrinsic = Intrinsic;

    fn endianness(&self) -> Endianness {
        Endianness::LittleEndian
    }

    fn address_size(&self) -> usize {
        4
    }

    fn default_integer_size(&self) -> usize {
        4
    }

    fn instruction_alignment(&self) -> usize {
        1
    }

    /// Call targets are 4-byte aligned in both ABIs.
    fn linear_sweep_initial_alignment(&self) -> usize {
        4
    }

    /// On the windowed cores, functions are found from call targets (and by
    /// the ESP view, from `entry` instructions) only. The generic control
    /// flow sweep starts a function right after each return, which on these
    /// cores is the padding before the next 4-byte aligned `entry`; on the
    /// ESP32-S3 that padding decodes as vector instructions.
    fn linear_sweep_analysis_capabilities(&self) -> u32 {
        let call_targets = LinearSweepAnalysisCapability::BNLinearSweepCallTargetAnalysis as u32;
        if self.core.windowed() {
            call_targets
        } else {
            call_targets
                | LinearSweepAnalysisCapability::BNLinearSweepGenericControlFlowAnalysis as u32
        }
    }

    fn max_instr_len(&self) -> usize {
        if self.isa.has(Features::ESP32S3_PIE) {
            4
        } else {
            3
        }
    }

    fn opcode_display_len(&self) -> usize {
        self.max_instr_len()
    }

    fn associated_arch_by_addr(&self, _addr: u64) -> CoreArchitecture {
        self.handle
    }

    fn instruction_info(&self, data: &[u8], addr: u64) -> Option<InstructionInfo> {
        guard("instruction_info", addr, || {
            let insn = self.decode(data, addr)?;
            let next = insn.next_address() as u64;
            let mut info = InstructionInfo::new(insn.len(), 0);
            match insn.control_flow() {
                ControlFlow::Sequential => {
                    if let Some(lbeg) = self.loop_back(addr, next) {
                        info.add_branch(BranchKind::True(lbeg));
                        info.add_branch(BranchKind::False(next));
                    }
                }
                ControlFlow::Branch { target } => {
                    info.add_branch(BranchKind::True(target as u64));
                    info.add_branch(BranchKind::False(next));
                    if let Some(lbeg) = self.loop_back(addr, next) {
                        info.add_branch(BranchKind::True(lbeg));
                    }
                }
                ControlFlow::Jump { target } => {
                    info.add_branch(BranchKind::Unconditional(target as u64))
                }
                ControlFlow::IndirectJump => info.add_branch(BranchKind::Indirect),
                ControlFlow::Call { target, .. } => {
                    info.add_branch(BranchKind::Call(target as u64))
                }
                ControlFlow::IndirectCall { .. } => {}
                ControlFlow::Return | ControlFlow::ExceptionReturn => {
                    info.add_branch(BranchKind::FunctionReturn)
                }
                ControlFlow::Loop { end, conditional } => {
                    if let Some(loops) = &self.loops {
                        loops.record(next, end as u64);
                    }
                    if conditional {
                        info.add_branch(BranchKind::True(end as u64));
                        info.add_branch(BranchKind::False(next));
                    } else if let Some(lbeg) = self.loop_back(addr, next) {
                        // A loop instruction ending an enclosing loop's body.
                        info.add_branch(BranchKind::True(lbeg));
                        info.add_branch(BranchKind::False(next));
                    }
                }
                ControlFlow::Trap => match insn.opcode() {
                    xtensa_isa::Opcode::Syscall => info.add_branch(BranchKind::SystemCall),
                    xtensa_isa::Opcode::Ill | xtensa_isa::Opcode::IllN => {
                        info.add_branch(BranchKind::Exception)
                    }
                    _ => {}
                },
            }
            Some(info)
        })
    }

    fn instruction_text(
        &self,
        data: &[u8],
        addr: u64,
    ) -> Option<(usize, Vec<InstructionTextToken>)> {
        guard("instruction_text", addr, || {
            let insn = self.decode(data, addr)?;
            Some((insn.len(), token::tokens(&insn)))
        })
    }

    fn instruction_llil(
        &self,
        data: &[u8],
        addr: u64,
        il: &LowLevelILMutableFunction,
    ) -> Option<(usize, bool)> {
        let insn = guard("decoding", addr, || self.decode(data, addr))?;
        let lifter = Lifter {
            il,
            isa: &self.isa,
            intrinsics: &self.intrinsics,
            loops: self.loops.as_ref(),
            insn: &insn,
        };
        // A lifter bug must not take the whole process down.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lifter.lift())).is_err() {
            tracing::error!("failed to lift {insn} at {addr:#x}");
            crate::lift::unimplemented(il);
        }
        Some((insn.len(), true))
    }

    fn registers_all(&self) -> Vec<Reg> {
        self.registers.clone()
    }

    fn register_from_id(&self, id: RegisterId) -> Option<Reg> {
        Reg::from_id(&self.isa, id)
    }

    fn registers_full_width(&self) -> Vec<Reg> {
        self.registers.clone()
    }

    fn registers_system(&self) -> Vec<Reg> {
        self.registers
            .iter()
            .copied()
            .filter(Reg::is_system)
            .collect()
    }

    fn stack_pointer_reg(&self) -> Option<Reg> {
        Some(Reg::SP)
    }

    fn link_reg(&self) -> Option<Reg> {
        Some(Reg::LR)
    }

    fn flags(&self) -> Vec<Flag> {
        if self.isa.has(Features::BOOLEAN) {
            Flag::all()
        } else {
            Vec::new()
        }
    }

    fn flag_from_id(&self, id: FlagId) -> Option<Flag> {
        Flag::from_id(id)
    }

    fn flag_write_from_id(&self, _id: FlagWriteId) -> Option<FlagWrite> {
        None
    }

    fn flag_class_from_id(&self, _id: FlagClassId) -> Option<FlagClass> {
        None
    }

    fn flag_group_from_id(&self, _id: FlagGroupId) -> Option<FlagGroup> {
        None
    }

    fn intrinsics(&self) -> Vec<Intrinsic> {
        let mut all: Vec<Intrinsic> = self.intrinsics.values().copied().collect();
        all.sort_by_key(|i| i.id);
        all
    }

    fn intrinsic_from_id(&self, id: IntrinsicId) -> Option<Intrinsic> {
        self.intrinsics.get(&id.0).copied()
    }

    // Patching. Assembling is not supported (`can_assemble` stays false).

    fn is_never_branch_patch_available(&self, data: &[u8], addr: u64) -> bool {
        patching("is_never_branch_patch_available", addr, || {
            patch::is_conditional_branch(&self.isa, data, addr)
        })
    }

    fn is_invert_branch_patch_available(&self, data: &[u8], addr: u64) -> bool {
        patching("is_invert_branch_patch_available", addr, || {
            patch::is_conditional_branch(&self.isa, data, addr)
        })
    }

    fn is_always_branch_patch_available(&self, data: &[u8], addr: u64) -> bool {
        patching("is_always_branch_patch_available", addr, || {
            patch::can_always_branch(&self.isa, data, addr)
        })
    }

    fn is_skip_and_return_value_patch_available(&self, data: &[u8], addr: u64) -> bool {
        patching("is_skip_and_return_value_patch_available", addr, || {
            patch::is_call(&self.isa, data, addr)
        })
    }

    fn convert_to_nop(&self, data: &mut [u8], addr: u64) -> bool {
        patching("convert_to_nop", addr, || {
            patch::convert_to_nop(&self.isa, data)
        })
    }

    fn invert_branch(&self, data: &mut [u8], addr: u64) -> bool {
        patching("invert_branch", addr, || {
            patch::invert_branch(&self.isa, data, addr)
        })
    }

    fn always_branch(&self, data: &mut [u8], addr: u64) -> bool {
        patching("always_branch", addr, || {
            patch::always_branch(&self.isa, data, addr)
        })
    }

    fn skip_and_return_value(&self, data: &mut [u8], addr: u64, value: u64) -> bool {
        patching("skip_and_return_value", addr, || {
            patch::skip_and_return_value(&self.isa, data, addr, value)
        })
    }

    fn handle(&self) -> Self::Handle {
        self.custom_handle
    }
}
