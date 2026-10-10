//! Register model.
//!
//! Register ids are laid out so that every register class has a fixed range:
//!
//! | ids             | registers                          | size |
//! |-----------------|------------------------------------|------|
//! | `0..16`         | address registers `a0`..`a15`      | 4    |
//! | `16..32`        | FPU registers `f0`..`f15`          | 4    |
//! | `32..40`        | ESP32-S3 vector registers `q0`..`q7` | 16 |
//! | `0x100 + n`     | special register number `n`        | 4    |
//! | `0x200 + n`     | user register number `n`           | 4    |
//!
//! The boolean registers `b0`..`b15` are modelled as flags (see `flags.rs`).
//! The MAC16 data registers `m0`..`m3` are special registers 32..35 and are
//! represented as such.

use std::borrow::Cow;

use binaryninja::architecture::{self, ImplicitRegisterExtend, RegisterId};
use binaryninja::low_level_il::LowLevelILRegisterKind;
use xtensa_isa::{Isa, SpecialRegister, UserRegister};

const F_BASE: u32 = 16;
const Q_BASE: u32 = 32;
const SR_BASE: u32 = 0x100;
const UR_BASE: u32 = 0x200;

/// Special register numbers referenced by the lifter.
pub mod sr {
    pub const LBEG: u8 = 0;
    pub const LEND: u8 = 1;
    pub const LCOUNT: u8 = 2;
    pub const SAR: u8 = 3;
    pub const BR: u8 = 4;
    pub const SCOMPARE1: u8 = 12;
    pub const ACCLO: u8 = 16;
    pub const ACCHI: u8 = 17;
    pub const M0: u8 = 32;
    pub const PS: u8 = 230;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reg {
    A(u8),
    F(u8),
    Q(u8),
    Sr(&'static SpecialRegister),
    Ur(&'static UserRegister),
}

impl Reg {
    pub const SP: Reg = Reg::A(1);
    pub const LR: Reg = Reg::A(0);

    pub fn from_id(isa: &Isa, id: RegisterId) -> Option<Reg> {
        let id = id.0;
        match id {
            0..F_BASE => Some(Reg::A(id as u8)),
            F_BASE..Q_BASE => Some(Reg::F((id - F_BASE) as u8)),
            Q_BASE..40 => Some(Reg::Q((id - Q_BASE) as u8)),
            SR_BASE..UR_BASE => special_register(isa, (id - SR_BASE) as u8).map(Reg::Sr),
            UR_BASE..0x300 => user_register(isa, (id - UR_BASE) as u8).map(Reg::Ur),
            _ => None,
        }
    }

    /// Every register implemented by a core.
    pub fn all(isa: &Isa) -> Vec<Reg> {
        let mut regs: Vec<Reg> = (0..16).map(Reg::A).collect();
        if isa.has(xtensa_isa::Features::FP) {
            regs.extend((0..16).map(Reg::F));
        }
        if isa.has(xtensa_isa::Features::ESP32S3_PIE) {
            regs.extend((0..8).map(Reg::Q));
        }
        regs.extend(isa.special_registers.iter().map(Reg::Sr));
        regs.extend(isa.user_registers.iter().map(Reg::Ur));
        regs
    }

    /// Registers that hold machine state rather than program data. The
    /// decompiler does not consider these as function parameters or
    /// return values.
    pub fn is_system(&self) -> bool {
        match self {
            Reg::Sr(sr) => !matches!(
                sr.number,
                sr::LBEG
                    | sr::LEND
                    | sr::LCOUNT
                    | sr::SAR
                    | sr::BR
                    | sr::SCOMPARE1
                    | sr::ACCLO
                    | sr::ACCHI
                    | 32..=35
            ),
            Reg::Ur(ur) => !matches!(ur.name, "threadptr" | "fcr" | "fsr"),
            _ => false,
        }
    }

    pub fn size(&self) -> usize {
        match self {
            Reg::Q(_) => 16,
            _ => 4,
        }
    }

    pub fn by_name(isa: &Isa, name: &str) -> Option<Reg> {
        Reg::all(isa).into_iter().find(|r| r.reg_name() == name)
    }

    pub fn reg_name(&self) -> Cow<'static, str> {
        match *self {
            Reg::A(n) => format!("a{n}").into(),
            Reg::F(n) => format!("f{n}").into(),
            Reg::Q(n) => format!("q{n}").into(),
            Reg::Sr(sr) => sr.name.into(),
            Reg::Ur(ur) => ur.name.into(),
        }
    }
}

pub fn special_register(isa: &Isa, number: u8) -> Option<&'static SpecialRegister> {
    isa.special_registers.iter().find(|sr| sr.number == number)
}

pub fn special_register_by_name(isa: &Isa, name: &str) -> Option<&'static SpecialRegister> {
    isa.special_registers.iter().find(|sr| sr.name == name)
}

pub fn user_register(isa: &Isa, number: u8) -> Option<&'static UserRegister> {
    isa.user_registers.iter().find(|ur| ur.number == number)
}

impl architecture::Register for Reg {
    type InfoType = Self;

    fn name(&self) -> Cow<'_, str> {
        self.reg_name()
    }

    fn info(&self) -> Self::InfoType {
        *self
    }

    fn id(&self) -> RegisterId {
        RegisterId(match *self {
            Reg::A(n) => n as u32,
            Reg::F(n) => F_BASE + n as u32,
            Reg::Q(n) => Q_BASE + n as u32,
            Reg::Sr(sr) => SR_BASE + sr.number as u32,
            Reg::Ur(ur) => UR_BASE + ur.number as u32,
        })
    }
}

impl architecture::RegisterInfo for Reg {
    type RegType = Self;

    fn parent(&self) -> Option<Self::RegType> {
        None
    }

    fn size(&self) -> usize {
        Reg::size(self)
    }

    fn offset(&self) -> usize {
        0
    }

    fn implicit_extend(&self) -> ImplicitRegisterExtend {
        ImplicitRegisterExtend::NoExtend
    }
}

impl From<Reg> for LowLevelILRegisterKind<Reg> {
    fn from(reg: Reg) -> Self {
        LowLevelILRegisterKind::Arch(reg)
    }
}
