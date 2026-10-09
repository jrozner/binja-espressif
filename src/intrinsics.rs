//! Intrinsics.
//!
//! Every opcode whose semantics are not expressed directly in LLIL is lifted
//! as an intrinsic named after its mnemonic (`memw` becomes `__memw`,
//! `ee.vadds.s16` becomes `__ee_vadds_s16`). The registers an intrinsic reads
//! and writes are worked out from the instruction's operands by
//! [`classify`].
//!
//! Binary Ninja declares an intrinsic's inputs and outputs once, per
//! intrinsic, rather than per instruction. Because an opcode always has the
//! same operand kinds, the declaration is derived by classifying one
//! exemplar encoding of the opcode (see `exemplars.rs`).

use std::borrow::Cow;
use std::collections::HashMap;

use binaryninja::architecture::{self, IntrinsicId};
use binaryninja::confidence::{Conf, MAX_CONFIDENCE};
use binaryninja::rc::Ref;
use binaryninja::types::{NameAndType, Type};
use xtensa::{Instruction, Isa, Opcode, Operand};

use crate::exemplars::EXEMPLARS;
use crate::registers::{Reg, special_register, sr};

/// A value an intrinsic consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Operand(Operand),
    Reg(Reg),
}

/// The registers an instruction lifted as an intrinsic writes and the values
/// it reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Io {
    pub outputs: Vec<Reg>,
    pub inputs: Vec<Input>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int(usize),
    Float,
    Bool,
}

#[derive(Debug)]
pub struct Signature {
    name: String,
    inputs: Vec<Kind>,
    outputs: Vec<Kind>,
}

#[derive(Debug, Clone, Copy)]
pub struct Intrinsic {
    pub id: u32,
    sig: &'static Signature,
}

/// The intrinsic id of an opcode: its index in [`Opcode::ALL`].
pub fn id_of(opcode: Opcode) -> IntrinsicId {
    IntrinsicId(Opcode::ALL.binary_search(&opcode).expect("opcode in ALL") as u32)
}

/// How an instruction accesses a special or user register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysAccess {
    Read,
    Write,
    Exchange,
}

const SYS_BASE: u32 = 0x1_0000;

/// The intrinsic id of an access to a system special or user register:
/// `__rsr_ccount`, `__wsr_intenable`, `__xsr_vecbase`, `__rur_gpio_out`, ...
pub fn sys_id(reg: Reg, access: SysAccess) -> Option<u32> {
    let (class, number) = match reg {
        Reg::Sr(sr) => (0, sr.number),
        Reg::Ur(ur) => (1, ur.number),
        _ => return None,
    };
    Some(SYS_BASE | (access as u32) << 12 | class << 8 | number as u32)
}

fn sys_intrinsic(reg: Reg, access: SysAccess) -> Option<(u32, Intrinsic)> {
    let id = sys_id(reg, access)?;
    let prefix = match (reg, access) {
        (Reg::Sr(_), SysAccess::Read) => "rsr",
        (Reg::Sr(_), SysAccess::Write) => "wsr",
        (Reg::Sr(_), SysAccess::Exchange) => "xsr",
        (_, SysAccess::Read) => "rur",
        (_, SysAccess::Write) => "wur",
        (_, SysAccess::Exchange) => return None,
    };
    let reads = access != SysAccess::Read;
    let writes = access != SysAccess::Write;
    let sig = Box::leak(Box::new(Signature {
        name: format!("__{prefix}_{}", reg.reg_name()),
        inputs: if reads { vec![Kind::Int(4)] } else { vec![] },
        outputs: if writes { vec![Kind::Int(4)] } else { vec![] },
    }));
    Some((id, Intrinsic { id, sig }))
}

/// Builds the intrinsics of a core: one per opcode with an exemplar.
pub fn build(isa: &Isa) -> HashMap<u32, Intrinsic> {
    let mut map = HashMap::new();
    for &(word, len) in EXEMPLARS {
        let bytes = word.to_le_bytes();
        let Ok(insn) = isa.decode(&bytes[..len as usize], 0) else {
            continue;
        };
        let id = id_of(insn.opcode()).0;
        if map.contains_key(&id) || !crate::lift::uses_intrinsic(insn.opcode()) {
            continue;
        }
        let io = classify(&insn, isa);
        let sig = Box::leak(Box::new(Signature {
            name: name_of(insn.opcode()),
            inputs: io.inputs.iter().map(input_kind).collect(),
            outputs: io.outputs.iter().map(reg_kind).collect(),
        }));
        map.insert(id, Intrinsic { id, sig });
    }
    for &reg in Reg::all(isa).iter().filter(|r| r.is_system()) {
        let accesses: Vec<SysAccess> = match reg {
            Reg::Sr(sr) => [
                (sr.read, SysAccess::Read),
                (sr.write, SysAccess::Write),
                (sr.exchange, SysAccess::Exchange),
            ]
            .into_iter()
            .filter_map(|(ok, access)| ok.then_some(access))
            .collect(),
            _ => vec![SysAccess::Read, SysAccess::Write],
        };
        for access in accesses {
            if let Some((id, intrinsic)) = sys_intrinsic(reg, access) {
                map.insert(id, intrinsic);
            }
        }
    }
    map
}

fn name_of(opcode: Opcode) -> String {
    format!("__{}", opcode.mnemonic().replace('.', "_"))
}

fn reg_kind(reg: &Reg) -> Kind {
    match reg {
        Reg::F(_) => Kind::Float,
        r => Kind::Int(r.size()),
    }
}

fn input_kind(input: &Input) -> Kind {
    match input {
        Input::Reg(r) => reg_kind(r),
        Input::Operand(op) => match op {
            Operand::Fr(_) => Kind::Float,
            Operand::Qr(_) => Kind::Int(16),
            Operand::Br(_) | Operand::BrGroup { .. } => Kind::Bool,
            _ => Kind::Int(4),
        },
    }
}

fn kind_type(kind: Kind) -> Ref<Type> {
    match kind {
        Kind::Int(size) => Type::int(size, false),
        Kind::Float => Type::float(4),
        Kind::Bool => Type::bool(),
    }
}

impl architecture::Intrinsic for Intrinsic {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.sig.name)
    }

    fn id(&self) -> IntrinsicId {
        IntrinsicId(self.id)
    }

    fn inputs(&self) -> Vec<NameAndType> {
        self.sig
            .inputs
            .iter()
            .map(|&k| NameAndType::new("", Conf::new(kind_type(k), MAX_CONFIDENCE)))
            .collect()
    }

    fn outputs(&self) -> Vec<Conf<Ref<Type>>> {
        self.sig
            .outputs
            .iter()
            .map(|&k| Conf::new(kind_type(k), MAX_CONFIDENCE))
            .collect()
    }
}

/// Converts an operand that names a register to that register.
pub fn operand_reg(isa: &Isa, op: &Operand) -> Option<Reg> {
    match *op {
        Operand::Ar(n) => Some(Reg::A(n)),
        Operand::Fr(n) => Some(Reg::F(n)),
        Operand::Qr(n) => Some(Reg::Q(n)),
        Operand::Mr(n) => special_register(isa, sr::M0 + n).map(Reg::Sr),
        Operand::Sr(s) => Some(Reg::Sr(s)),
        Operand::Ur(u) => Some(Reg::Ur(u)),
        _ => None,
    }
}

fn acc(isa: &Isa) -> Vec<Reg> {
    [sr::ACCLO, sr::ACCHI]
        .into_iter()
        .filter_map(|n| special_register(isa, n).map(Reg::Sr))
        .collect()
}

/// Works out which registers an instruction lifted as an intrinsic writes
/// and which values it reads.
///
/// The classification depends only on operand positions and kinds, never on
/// register numbers, so that it agrees with the exemplar-derived declaration.
pub fn classify(insn: &Instruction, isa: &Isa) -> Io {
    use Opcode as O;
    let ops = insn.operands();
    let mnemonic = insn.opcode().mnemonic();
    let all_inputs = |ops: &[Operand]| ops.iter().copied().map(Input::Operand).collect();

    // MAC16: the multiplies write the 40-bit accumulator; the `mula`/`muls`
    // forms also read it. The `.ldinc`/`.lddec` forms' load and address
    // update are lifted separately; the intrinsic covers the multiply.
    if mnemonic.starts_with("mul.")
        || mnemonic.starts_with("mula.")
        || mnemonic.starts_with("muls.")
        || mnemonic.starts_with("umul.")
    {
        let acc = acc(isa);
        let sources = if mnemonic.ends_with(".ldinc") || mnemonic.ends_with(".lddec") {
            &ops[2..]
        } else {
            ops
        };
        let mut inputs: Vec<Input> = all_inputs(sources);
        if mnemonic.starts_with("mula.") || mnemonic.starts_with("muls.") {
            inputs.extend(acc.iter().copied().map(Input::Reg));
        }
        return Io {
            outputs: acc,
            inputs,
        };
    }

    if mnemonic.starts_with("ee.") {
        return classify_pie(insn, isa);
    }

    match insn.opcode() {
        // Operands that are only read.
        O::Idtlb
        | O::Iitlb
        | O::Wdtlb
        | O::Witlb
        | O::Wer
        | O::Wf64r
        | O::WrmskExpstate
        | O::WrMaskGpioOut => Io {
            outputs: vec![],
            inputs: all_inputs(ops),
        },
        // The first operand is read and written.
        O::Lddr32P | O::Sddr32P | O::DivnS | O::AddexpS | O::AddexpmS | O::MkdadjS | O::MksadjS => {
            Io {
                outputs: operand_reg(isa, &ops[0]).into_iter().collect(),
                inputs: all_inputs(ops),
            }
        }
        // By default the first operand, if it is a register, is the result.
        _ => match ops.first().and_then(|op| operand_reg(isa, op)) {
            Some(reg) => Io {
                outputs: vec![reg],
                inputs: all_inputs(&ops[1..]),
            },
            None => Io {
                outputs: vec![],
                inputs: all_inputs(ops),
            },
        },
    }
}

/// The ESP32-S3 PIE instructions have hundreds of variants that are
/// classified from their names: the `.ip`, `.xp`, `.incp` and `.decp` forms
/// update their address register, and the store forms (`.st.`, `vst`, `stf`,
/// ...) only read their vector operands. Every register operand is passed as
/// an input, since many of these instructions accumulate into their
/// destination. Accesses to the implicit `qacc`/`accx`/`sar_byte`/`ua_state`
/// state and to memory are not described.
fn classify_pie(insn: &Instruction, isa: &Isa) -> Io {
    let ops = insn.operands();
    let mnemonic = insn.opcode().mnemonic();
    let segments: Vec<&str> = mnemonic.split('.').collect();
    let inputs = ops.iter().copied().map(Input::Operand).collect();
    let reg = |i: usize| ops.get(i).and_then(|op| operand_reg(isa, op));

    let updates_ptr = segments
        .iter()
        .any(|s| matches!(*s, "ip" | "xp" | "incp" | "decp"));
    let ptr = if updates_ptr {
        ops.iter().position(|op| matches!(op, Operand::Ar(_)))
    } else {
        None
    };
    let pure_store = matches!(segments.get(1), Some(&("st" | "vst" | "stf" | "stxq")))
        || mnemonic.starts_with("ee.fft.vst");
    let store = pure_store || segments.contains(&"st");

    let mut outputs: Vec<Reg> = Vec::new();
    match mnemonic {
        "ee.movi.32.a" => outputs.extend(reg(1)),
        "ee.wr_mask_gpio_out" => {}
        m if m.starts_with("ee.vzip")
            || m.starts_with("ee.vunzip")
            || m.ends_with(".2q")
            || m == "ee.src.q.qup" =>
        {
            outputs.extend(reg(0));
            outputs.extend(reg(1));
        }
        _ if pure_store => {}
        _ if store => {
            // Compute-and-store: the result is the first vector register
            // after the address register.
            if let Some(p) = ptr {
                outputs.extend(
                    ops[p + 1..]
                        .iter()
                        .find(|op| matches!(op, Operand::Qr(_)))
                        .and_then(|op| operand_reg(isa, op)),
                );
            }
        }
        _ => match ptr {
            Some(p) => {
                // Loads write every register before the address register;
                // compute-and-load forms also write the vector register right
                // after it when it is followed by the two sources.
                outputs.extend(ops[..p].iter().filter_map(|op| operand_reg(isa, op)));
                let after = &ops[p + 1..];
                if matches!(after.first(), Some(Operand::Qr(_)))
                    && after[1..]
                        .iter()
                        .filter(|op| matches!(op, Operand::Qr(_)))
                        .count()
                        >= 2
                {
                    outputs.extend(operand_reg(isa, &after[0]));
                }
            }
            None => outputs.extend(reg(0)),
        },
    }
    if let Some(p) = ptr {
        outputs.extend(reg(p));
    }
    Io { outputs, inputs }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_opcode_has_an_exemplar() {
        // Opcodes without an exemplar would have no intrinsic declaration.
        let mut found = std::collections::HashSet::new();
        for isa in [Isa::ESP8266, Isa::ESP32, Isa::ESP32S2, Isa::ESP32S3] {
            for (id, _) in build(&isa) {
                if let Some(&op) = Opcode::ALL.get(id as usize) {
                    found.insert(op);
                }
            }
        }
        let missing: Vec<_> = Opcode::ALL
            .iter()
            .filter(|&&o| crate::lift::uses_intrinsic(o) && !found.contains(&o))
            .collect();
        assert!(missing.is_empty(), "missing exemplars: {missing:?}");
    }

    fn exemplar(isa: &Isa, mnemonic: &str) -> Instruction {
        EXEMPLARS
            .iter()
            .filter_map(|&(w, l)| isa.decode(&w.to_le_bytes()[..l as usize], 0).ok())
            .find(|i| i.opcode().mnemonic() == mnemonic)
            .unwrap()
    }

    #[test]
    fn pie_classification() {
        let isa = Isa::ESP32S3;
        let outs = |m: &str| classify(&exemplar(&isa, m), &isa).outputs;
        let (q, a) = (Reg::Q(0), Reg::A(0));
        assert_eq!(outs("ee.vld.128.ip"), [q, a]);
        assert_eq!(outs("ee.vst.128.ip"), [a]);
        assert_eq!(outs("ee.vadds.s16"), [q]);
        assert_eq!(outs("ee.vadds.s16.ld.incp"), [q, q, a]);
        assert_eq!(outs("ee.vadds.s16.st.incp"), [q, a]);
        assert_eq!(outs("ee.vmulas.s16.accx.ld.ip"), [q, a]);
        assert_eq!(
            outs("ee.ldf.128.ip"),
            [Reg::F(0); 4].into_iter().chain([a]).collect::<Vec<_>>()
        );
        assert_eq!(outs("ee.movi.32.a"), [a]);
        assert_eq!(outs("ee.st.accx.ip"), [a]);
        assert_eq!(outs("ee.wr_mask_gpio_out"), []);
        assert_eq!(outs("ee.zero.accx"), []);
    }
}
