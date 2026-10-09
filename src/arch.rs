//! The Binary Ninja architecture.

use std::collections::HashMap;

use binaryninja::Endianness;
use binaryninja::architecture::{
    Architecture, BranchKind, CoreArchitecture, CustomArchitectureHandle, FlagClassId, FlagGroupId,
    FlagId, FlagWriteId, InstructionInfo, IntrinsicId, RegisterId, UnusedRegisterStack,
};
use binaryninja::disassembly::InstructionTextToken;
use binaryninja::low_level_il::LowLevelILMutableFunction;
use binaryninjacore_sys::BNLinearSweepAnalysisCapability as LinearSweepAnalysisCapability;
use xtensa::{ControlFlow, Features, Instruction, Isa};

use crate::flags::{Flag, FlagClass, FlagGroup, FlagWrite};
use crate::intrinsics::{self, Intrinsic};
use crate::lift::Lifter;
use crate::loops::Loops;
use crate::registers::Reg;
use crate::tokens;

/// A supported core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chip {
    Esp8266,
    Esp32,
    Esp32S2,
    Esp32S3,
}

impl Chip {
    pub const ALL: [Chip; 4] = [Chip::Esp8266, Chip::Esp32, Chip::Esp32S2, Chip::Esp32S3];

    pub fn isa(self) -> Isa {
        match self {
            Chip::Esp8266 => Isa::ESP8266,
            Chip::Esp32 => Isa::ESP32,
            Chip::Esp32S2 => Isa::ESP32S2,
            Chip::Esp32S3 => Isa::ESP32S3,
        }
    }

    /// The architecture (and standalone platform) name.
    pub fn arch_name(self) -> &'static str {
        match self {
            Chip::Esp8266 => "xtensa-esp8266",
            Chip::Esp32 => "xtensa-esp32",
            Chip::Esp32S2 => "xtensa-esp32s2",
            Chip::Esp32S3 => "xtensa-esp32s3",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Chip::Esp8266 => "ESP8266",
            Chip::Esp32 => "ESP32",
            Chip::Esp32S2 => "ESP32-S2",
            Chip::Esp32S3 => "ESP32-S3",
        }
    }

    pub fn windowed(self) -> bool {
        self.isa().has(Features::WINDOWED)
    }
}

pub struct Xtensa {
    handle: CoreArchitecture,
    custom_handle: CustomArchitectureHandle<Xtensa>,
    pub chip: Chip,
    isa: Isa,
    intrinsics: HashMap<u32, Intrinsic>,
    registers: Vec<Reg>,
    /// Zero-overhead loops seen so far; `None` without the loop option.
    loops: Option<Loops>,
}

impl Xtensa {
    pub fn new(
        chip: Chip,
        handle: CoreArchitecture,
        custom_handle: CustomArchitectureHandle<Xtensa>,
    ) -> Self {
        let isa = chip.isa();
        Xtensa {
            handle,
            custom_handle,
            chip,
            isa,
            intrinsics: intrinsics::build(&isa),
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
        if self.chip.windowed() {
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
            ControlFlow::Call { target, .. } => info.add_branch(BranchKind::Call(target as u64)),
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
                xtensa::Opcode::Syscall => info.add_branch(BranchKind::SystemCall),
                xtensa::Opcode::Ill | xtensa::Opcode::IllN => {
                    info.add_branch(BranchKind::Exception)
                }
                _ => {}
            },
        }
        Some(info)
    }

    fn instruction_text(
        &self,
        data: &[u8],
        addr: u64,
    ) -> Option<(usize, Vec<InstructionTextToken>)> {
        let insn = self.decode(data, addr)?;
        Some((insn.len(), tokens::tokens(&insn)))
    }

    fn instruction_llil(
        &self,
        data: &[u8],
        addr: u64,
        il: &LowLevelILMutableFunction,
    ) -> Option<(usize, bool)> {
        let insn = self.decode(data, addr)?;
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

    fn handle(&self) -> Self::Handle {
        self.custom_handle
    }
}
