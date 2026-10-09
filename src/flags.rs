//! Flags.
//!
//! Xtensa has no condition codes: integer comparisons are fused into the
//! branch instructions. The only architectural single-bit state is the
//! Boolean option's sixteen `b0`..`b15` registers, set by floating point
//! compares and the boolean logic instructions and consumed by `bt`/`bf`,
//! `movt`/`movf` and their `.s` variants. Those map naturally onto Binary
//! Ninja flags, so each boolean register is a flag whose id is its number.
//!
//! None of the instructions write flags implicitly, so there are no flag
//! write types, flag classes or flag groups.

use std::borrow::Cow;
use std::collections::HashMap;

use binaryninja::architecture::{
    self, FlagClassId, FlagCondition, FlagGroupId, FlagId, FlagRole, FlagWriteId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Flag(pub u8);

impl Flag {
    pub fn all() -> Vec<Flag> {
        (0..16).map(Flag).collect()
    }

    pub fn from_id(id: FlagId) -> Option<Flag> {
        (id.0 < 16).then_some(Flag(id.0 as u8))
    }
}

impl architecture::Flag for Flag {
    type FlagClass = FlagClass;

    fn name(&self) -> Cow<'_, str> {
        format!("b{}", self.0).into()
    }

    fn role(&self, _class: Option<Self::FlagClass>) -> FlagRole {
        FlagRole::SpecialFlagRole
    }

    fn id(&self) -> FlagId {
        FlagId(self.0 as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlagClass {}

impl architecture::FlagClass for FlagClass {
    fn name(&self) -> Cow<'_, str> {
        match *self {}
    }

    fn id(&self) -> FlagClassId {
        match *self {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlagWrite {}

impl architecture::FlagWrite for FlagWrite {
    type FlagType = Flag;
    type FlagClass = FlagClass;

    fn name(&self) -> Cow<'_, str> {
        match *self {}
    }

    fn class(&self) -> Option<Self::FlagClass> {
        match *self {}
    }

    fn id(&self) -> FlagWriteId {
        match *self {}
    }

    fn flags_written(&self) -> Vec<Self::FlagType> {
        match *self {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlagGroup {}

impl architecture::FlagGroup for FlagGroup {
    type FlagType = Flag;
    type FlagClass = FlagClass;

    fn name(&self) -> Cow<'_, str> {
        match *self {}
    }

    fn id(&self) -> FlagGroupId {
        match *self {}
    }

    fn flags_required(&self) -> Vec<Self::FlagType> {
        match *self {}
    }

    fn flag_conditions(&self) -> HashMap<Self::FlagClass, FlagCondition> {
        match *self {}
    }
}
