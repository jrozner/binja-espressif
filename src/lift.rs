//! Low Level IL lifting.

use std::collections::HashMap;

use binaryninja::low_level_il::expression::ValueExpr;
use binaryninja::low_level_il::lifting::LowLevelILLabel;
use binaryninja::low_level_il::{
    LowLevelILMutableExpression, LowLevelILMutableFunction, LowLevelILTempRegister,
};
use xtensa::{Instruction, Isa, Opcode as O, Operand};

use crate::flags::Flag;
use crate::intrinsics::{self, Input, Intrinsic, SysAccess, operand_reg};
use crate::loops::Loops;
use crate::registers::{Reg, special_register, special_register_by_name, sr};

type Il = LowLevelILMutableFunction;

/// The number of times lifting has fallen back to an unimplemented
/// expression, for the integration tests.
pub static UNIMPLEMENTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Emits an unimplemented instruction.
pub(crate) fn unimplemented(il: &Il) {
    UNIMPLEMENTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    il.unimplemented().append();
}
type Expr<'a> = LowLevelILMutableExpression<'a, ValueExpr>;

/// Whether an opcode is lifted, at least in part, as an intrinsic. Everything
/// not listed here has native LLIL semantics in [`Lifter::lift`].
pub fn uses_intrinsic(op: O) -> bool {
    let m = op.mnemonic();
    if m.ends_with(".ldinc") || m.ends_with(".lddec") {
        return true;
    }
    !matches!(
        op,
        O::Nop
            | O::NopN
            | O::Add
            | O::AddN
            | O::Addi
            | O::AddiN
            | O::Addmi
            | O::Addx2
            | O::Addx4
            | O::Addx8
            | O::Subx2
            | O::Subx4
            | O::Subx8
            | O::Sub
            | O::Neg
            | O::Abs
            | O::And
            | O::Or
            | O::Xor
            | O::MovN
            | O::Movsp
            | O::Movi
            | O::MoviN
            | O::Min
            | O::Max
            | O::Minu
            | O::Maxu
            | O::Salt
            | O::Saltu
            | O::Sext
            | O::Clamps
            | O::Nsa
            | O::Nsau
            | O::Mul16u
            | O::Mul16s
            | O::Mull
            | O::Muluh
            | O::Mulsh
            | O::Quos
            | O::Quou
            | O::Rems
            | O::Remu
            | O::Moveqz
            | O::Movnez
            | O::Movltz
            | O::Movgez
            | O::Movt
            | O::Movf
            | O::Slli
            | O::Srli
            | O::Srai
            | O::Extui
            | O::Ssl
            | O::Ssr
            | O::Ssai
            | O::Ssa8l
            | O::Ssa8b
            | O::Sll
            | O::Srl
            | O::Sra
            | O::Src
            | O::L8ui
            | O::L16ui
            | O::L16si
            | O::L32i
            | O::L32iN
            | O::L32ai
            | O::L32e
            | O::L32r
            | O::S8i
            | O::S16i
            | O::S32i
            | O::S32iN
            | O::S32ri
            | O::S32e
            | O::S32nb
            | O::S32c1i
            | O::Beq
            | O::Bne
            | O::Blt
            | O::Bge
            | O::Bltu
            | O::Bgeu
            | O::Beqi
            | O::Bnei
            | O::Blti
            | O::Bgei
            | O::Bltui
            | O::Bgeui
            | O::Beqz
            | O::BeqzN
            | O::Bnez
            | O::BnezN
            | O::Bltz
            | O::Bgez
            | O::Ball
            | O::Bnall
            | O::Bany
            | O::Bnone
            | O::Bbc
            | O::Bbs
            | O::Bbci
            | O::Bbsi
            | O::Bt
            | O::Bf
            | O::J
            | O::Jx
            | O::Call0
            | O::Callx0
            | O::Call4
            | O::Call8
            | O::Call12
            | O::Callx4
            | O::Callx8
            | O::Callx12
            | O::Ret
            | O::RetN
            | O::Retw
            | O::RetwN
            | O::Entry
            | O::Loop
            | O::Loopnez
            | O::Loopgtz
            | O::Rsr
            | O::Rur
            | O::Wsr
            | O::Wur
            | O::Xsr
            | O::Syscall
            | O::Break
            | O::BreakN
            | O::Ill
            | O::IllN
            | O::Andb
            | O::Andbc
            | O::Orb
            | O::Orbc
            | O::Xorb
            | O::Any4
            | O::All4
            | O::Any8
            | O::All8
            | O::AddS
            | O::SubS
            | O::MulS
            | O::MaddS
            | O::MaddnS
            | O::MsubS
            | O::NegS
            | O::AbsS
            | O::MovS
            | O::Rfr
            | O::Wfr
            | O::FloatS
            | O::UfloatS
            | O::TruncS
            | O::UtruncS
            | O::RoundS
            | O::FloorS
            | O::CeilS
            | O::OeqS
            | O::OltS
            | O::OleS
            | O::UnS
            | O::UeqS
            | O::UltS
            | O::UleS
            | O::MoveqzS
            | O::MovnezS
            | O::MovltzS
            | O::MovgezS
            | O::MovtS
            | O::MovfS
            | O::Lsi
            | O::Lsx
            | O::Ssi
            | O::Ssx
            | O::Lsip
            | O::Lsxp
            | O::Ssip
            | O::Ssxp
            | O::Ldinc
            | O::Lddec
            | O::LdQr
            | O::StQr
            | O::MvQr
            | O::EeZeroQ
            | O::EeVld128Ip
            | O::EeVld128Xp
            | O::EeVst128Ip
            | O::EeVst128Xp
            | O::EeAndq
            | O::EeOrq
            | O::EeXorq
            | O::EeNotq
    )
}

pub struct Lifter<'a> {
    pub il: &'a Il,
    pub isa: &'a Isa,
    pub intrinsics: &'a HashMap<u32, Intrinsic>,
    pub loops: Option<&'a Loops>,
    pub insn: &'a Instruction,
}

fn temp(n: u32) -> LowLevelILTempRegister {
    LowLevelILTempRegister::new(n)
}

impl<'a> Lifter<'a> {
    fn ops(&self) -> &'a [Operand] {
        self.insn.operands()
    }

    fn addr(&self) -> u64 {
        self.insn.address() as u64
    }

    fn next(&self) -> u64 {
        self.insn.next_address() as u64
    }

    /// The register number of operand `i`, which must be an address register.
    fn a(&self, i: usize) -> Reg {
        match self.ops()[i] {
            Operand::Ar(n) => Reg::A(n),
            op => panic!(
                "{}: operand {i} is {op:?}, not an address register",
                self.insn
            ),
        }
    }

    fn f(&self, i: usize) -> Reg {
        match self.ops()[i] {
            Operand::Fr(n) => Reg::F(n),
            op => panic!("{}: operand {i} is {op:?}, not an FP register", self.insn),
        }
    }

    fn b(&self, i: usize) -> Flag {
        match self.ops()[i] {
            Operand::Br(n) => Flag(n),
            op => panic!(
                "{}: operand {i} is {op:?}, not a boolean register",
                self.insn
            ),
        }
    }

    fn imm(&self, i: usize) -> i32 {
        match self.ops()[i] {
            Operand::Imm(v) => v,
            op => panic!("{}: operand {i} is {op:?}, not an immediate", self.insn),
        }
    }

    fn target(&self) -> u64 {
        match self.ops().last() {
            Some(Operand::Target(t)) => *t as u64,
            op => panic!("{}: last operand is {op:?}, not a target", self.insn),
        }
    }

    fn sr(&self, number: u8) -> Option<Reg> {
        special_register(self.isa, number).map(Reg::Sr)
    }

    fn reg(&self, r: Reg) -> Expr<'a> {
        self.il.reg(r.size(), r)
    }

    fn ar(&self, i: usize) -> Expr<'a> {
        self.reg(self.a(i))
    }

    fn fr(&self, i: usize) -> Expr<'a> {
        self.reg(self.f(i))
    }

    fn c(&self, v: i64) -> Expr<'a> {
        self.il.const_int(4, v as u64 & 0xffff_ffff)
    }

    fn set(&self, r: Reg, e: Expr<'a>) {
        self.il.set_reg(r.size(), r, e).append();
    }

    fn set_temp(&self, t: u32, size: usize, e: Expr<'a>) {
        self.il.set_reg(size, temp(t), e).append();
    }

    fn get_temp(&self, t: u32, size: usize) -> Expr<'a> {
        self.il.reg(size, temp(t))
    }

    /// `as + offset`
    fn mem(&self, base: usize, offset: i64) -> Expr<'a> {
        if offset == 0 {
            self.ar(base)
        } else {
            self.il.add(4, self.ar(base), self.c(offset)).build()
        }
    }

    fn operand_value(&self, op: &Operand) -> Expr<'a> {
        match *op {
            Operand::Imm(v) => self.c(v as i64),
            Operand::Target(t) | Operand::Literal(t) => self.il.const_ptr(t as u64),
            Operand::Br(n) => self.il.flag(Flag(n)),
            Operand::BrGroup { first, .. } => self.il.flag(Flag(first)),
            _ => match operand_reg(self.isa, op) {
                Some(r) => self.reg(r),
                None => self.il.undefined(),
            },
        }
    }

    fn label_or_new(&self, addr: u64) -> (LowLevelILLabel, bool) {
        match self.il.label_for_address(addr) {
            Some(l) => (l, false),
            None => (LowLevelILLabel::new(), true),
        }
    }

    /// Transfers control to `target`, preferring a label within the function.
    fn goto_addr(&self, target: u64) {
        match self.il.label_for_address(target) {
            Some(mut l) => self.il.goto(&mut l).append(),
            None => self.il.jump(self.il.const_ptr(target)).append(),
        }
    }

    /// Conditional branch to `target`, otherwise falling through (and
    /// applying the zero-overhead loop check if this ends a loop body).
    fn branch(&self, cond: Expr<'a>, target: u64) {
        let (mut t, new_t) = self.label_or_new(target);
        let loop_end = self.loop_back().is_some();
        let (mut f, new_f) = if loop_end {
            (LowLevelILLabel::new(), true)
        } else {
            self.label_or_new(self.next())
        };
        self.il.if_expr(cond, &mut t, &mut f).append();
        if new_t {
            self.il.mark_label(&mut t);
            self.il.jump(self.il.const_ptr(target)).append();
        }
        if new_f {
            self.il.mark_label(&mut f);
        }
        if loop_end {
            self.loop_check();
        }
    }

    /// The `LBEG` of the zero-overhead loop whose body this instruction ends.
    fn loop_back(&self) -> Option<u64> {
        self.loops?.loop_back(self.addr(), self.next())
    }

    /// The implicit branch back of a zero-overhead loop at the end of its
    /// body: `if (lcount != 0) { lcount -= 1; goto LBEG; }`.
    fn loop_check(&self) {
        let (Some(lbeg), Some(lcount)) = (self.loop_back(), self.sr(sr::LCOUNT)) else {
            return;
        };
        let mut back = LowLevelILLabel::new();
        let (mut exit, new_exit) = self.label_or_new(self.next());
        let cond = self.il.cmp_ne(4, self.reg(lcount), self.c(0));
        self.il.if_expr(cond, &mut back, &mut exit).append();
        self.il.mark_label(&mut back);
        self.set(lcount, self.il.sub(4, self.reg(lcount), self.c(1)).build());
        self.goto_addr(lbeg);
        if new_exit {
            self.il.mark_label(&mut exit);
        }
    }

    /// `if (cond) dest = value`
    fn cond_set(&self, cond: Expr<'a>, dest: Reg, value: Expr<'a>) {
        let mut t = LowLevelILLabel::new();
        let mut f = LowLevelILLabel::new();
        self.il.if_expr(cond, &mut t, &mut f).append();
        self.il.mark_label(&mut t);
        self.set(dest, value);
        self.il.goto(&mut f).append();
        self.il.mark_label(&mut f);
    }

    fn intrinsic(&self) {
        let id = intrinsics::id_of(self.insn.opcode()).0;
        let Some(&intrinsic) = self.intrinsics.get(&id) else {
            unimplemented(self.il);
            return;
        };
        let io = intrinsics::classify(self.insn, self.isa);
        let inputs: Vec<Expr<'a>> = io
            .inputs
            .iter()
            .map(|input| match input {
                Input::Operand(op) => self.operand_value(op),
                Input::Reg(r) => self.reg(*r),
            })
            .collect();
        self.il.intrinsic(io.outputs, intrinsic, inputs).append();
    }

    /// Windowed call. The `windowed` calling convention describes `call8`
    /// (arguments in `a10..a15`, result in `a10`/`a11`), which is all GCC
    /// emits. `call4` and `call12` rotate the window by 4 and 12 registers
    /// instead, so their arguments are moved into the `call8` registers
    /// before the call and the result is moved back afterwards. Their other
    /// effects on the caller's registers above `a(n)` are not modelled.
    fn windowed_call(&self, n: u8, target: Expr<'a>) {
        if n == 8 {
            self.il.call(target).append();
            return;
        }
        const TARGET: u32 = 0;
        self.set_temp(TARGET, 4, target);
        // Copy towards the destination's far end first so that overlapping
        // source registers are read before they are overwritten.
        let moves: Vec<(u8, u8)> = (0..6u8)
            .filter(|k| n + 2 + k <= 15)
            .map(|k| (10 + k, n + 2 + k))
            .collect();
        if n < 8 {
            for &(dst, src) in moves.iter().rev() {
                self.set(Reg::A(dst), self.reg(Reg::A(src)));
            }
        } else {
            for &(dst, src) in &moves {
                self.set(Reg::A(dst), self.reg(Reg::A(src)));
            }
        }
        self.il.call(self.get_temp(TARGET, 4)).append();
        self.set(Reg::A(n + 2), self.reg(Reg::A(10)));
        if n + 3 <= 15 {
            self.set(Reg::A(n + 3), self.reg(Reg::A(11)));
        }
    }

    fn load(&self, size: usize, signed: bool, dest: Reg, addr: Expr<'a>) {
        let value = self.il.load(size, addr).build();
        let value = match (size, signed) {
            (4, _) | (16, _) => value,
            (_, false) => self.il.zx(4, value).build(),
            (_, true) => self.il.sx(4, value).build(),
        };
        self.set(dest, value);
    }

    fn store(&self, size: usize, addr: Expr<'a>, value: Expr<'a>) {
        let value = if size < 4 {
            self.il.low_part(size, value).build()
        } else {
            value
        };
        self.il.store(size, addr, value).append();
    }

    /// The FP conversion scale factor `2^imm` as a float.
    fn scale(&self, imm: i32) -> Expr<'a> {
        self.il.int_to_float(4, self.c(1i64 << imm)).build()
    }

    fn exception_return(&self, epc: Option<Reg>) {
        self.intrinsic();
        match epc {
            Some(epc) => self.il.ret(self.reg(epc)).append(),
            None => self.il.no_ret().append(),
        }
    }

    pub fn lift(&self) {
        let il = self.il;
        let insn = self.insn;
        let ops = self.ops();

        match insn.opcode() {
            O::Nop | O::NopN => il.nop().append(),

            // Arithmetic
            O::Add | O::AddN => self.set(self.a(0), il.add(4, self.ar(1), self.ar(2)).build()),
            O::Addi | O::AddiN | O::Addmi => self.set(
                self.a(0),
                il.add(4, self.ar(1), self.c(self.imm(2) as i64)).build(),
            ),
            O::Addx2 | O::Addx4 | O::Addx8 | O::Subx2 | O::Subx4 | O::Subx8 => {
                let shift = match insn.opcode() {
                    O::Addx2 | O::Subx2 => 1,
                    O::Addx4 | O::Subx4 => 2,
                    _ => 3,
                };
                let scaled = il.lsl(4, self.ar(1), self.c(shift)).build();
                let value = match insn.opcode() {
                    O::Addx2 | O::Addx4 | O::Addx8 => il.add(4, scaled, self.ar(2)).build(),
                    _ => il.sub(4, scaled, self.ar(2)).build(),
                };
                self.set(self.a(0), value);
            }
            O::Sub => self.set(self.a(0), il.sub(4, self.ar(1), self.ar(2)).build()),
            O::Neg => self.set(self.a(0), il.neg(4, self.ar(1)).build()),
            O::Abs => self.set(self.a(0), il.abs(4, self.ar(1)).build()),
            O::And => self.set(self.a(0), il.and(4, self.ar(1), self.ar(2)).build()),
            O::Or => {
                // `or ar, as, as` is the canonical `mov`.
                if ops[1] == ops[2] {
                    self.set(self.a(0), self.ar(1));
                } else {
                    self.set(self.a(0), il.or(4, self.ar(1), self.ar(2)).build());
                }
            }
            O::Xor => self.set(self.a(0), il.xor(4, self.ar(1), self.ar(2)).build()),
            O::MovN | O::Movsp => self.set(self.a(0), self.ar(1)),
            O::Movi | O::MoviN => self.set(self.a(0), self.c(self.imm(1) as i64)),
            O::Min => self.set(self.a(0), il.min_signed(4, self.ar(1), self.ar(2)).build()),
            O::Max => self.set(self.a(0), il.max_signed(4, self.ar(1), self.ar(2)).build()),
            O::Minu => self.set(
                self.a(0),
                il.min_unsigned(4, self.ar(1), self.ar(2)).build(),
            ),
            O::Maxu => self.set(
                self.a(0),
                il.max_unsigned(4, self.ar(1), self.ar(2)).build(),
            ),
            O::Salt | O::Saltu => {
                let cmp = if insn.opcode() == O::Salt {
                    il.cmp_slt(4, self.ar(1), self.ar(2)).build()
                } else {
                    il.cmp_ult(4, self.ar(1), self.ar(2)).build()
                };
                self.set(self.a(0), il.bool_to_int(4, cmp).build());
            }
            O::Sext => {
                let t = self.imm(2);
                let value = match t {
                    7 => il.sx(4, il.low_part(1, self.ar(1))).build(),
                    15 => il.sx(4, il.low_part(2, self.ar(1))).build(),
                    _ => {
                        let s = self.c(31 - t as i64);
                        il.asr(4, il.lsl(4, self.ar(1), s), self.c(31 - t as i64))
                            .build()
                    }
                };
                self.set(self.a(0), value);
            }
            O::Clamps => {
                let t = self.imm(2) as i64;
                let lo = il.max_signed(4, self.ar(1), self.c(-(1 << t)));
                self.set(
                    self.a(0),
                    il.min_signed(4, lo, self.c((1 << t) - 1)).build(),
                );
            }
            O::Nsa => self.set(self.a(0), il.cls(4, self.ar(1)).build()),
            O::Nsau => self.set(self.a(0), il.clz(4, self.ar(1)).build()),
            O::Mul16u => {
                let l = il.and(4, self.ar(1), self.c(0xffff));
                let r = il.and(4, self.ar(2), self.c(0xffff));
                self.set(self.a(0), il.mul(4, l, r).build());
            }
            O::Mul16s => {
                let l = il.sx(4, il.low_part(2, self.ar(1)));
                let r = il.sx(4, il.low_part(2, self.ar(2)));
                self.set(self.a(0), il.mul(4, l, r).build());
            }
            O::Mull => self.set(self.a(0), il.mul(4, self.ar(1), self.ar(2)).build()),
            O::Muluh => {
                let wide = il.mulu_dp(4, self.ar(1), self.ar(2));
                let high = il.lsr(8, wide, il.const_int(1, 32));
                self.set(self.a(0), il.low_part(4, high).build());
            }
            O::Mulsh => {
                let wide = il.muls_dp(4, self.ar(1), self.ar(2));
                let high = il.asr(8, wide, il.const_int(1, 32));
                self.set(self.a(0), il.low_part(4, high).build());
            }
            O::Quos => self.set(self.a(0), il.divs(4, self.ar(1), self.ar(2)).build()),
            O::Quou => self.set(self.a(0), il.divu(4, self.ar(1), self.ar(2)).build()),
            O::Rems => self.set(self.a(0), il.mods(4, self.ar(1), self.ar(2)).build()),
            O::Remu => self.set(self.a(0), il.modu(4, self.ar(1), self.ar(2)).build()),

            // Conditional moves
            O::Moveqz | O::Movnez | O::Movltz | O::Movgez => {
                let cond = match insn.opcode() {
                    O::Moveqz => il.cmp_e(4, self.ar(2), self.c(0)).build(),
                    O::Movnez => il.cmp_ne(4, self.ar(2), self.c(0)).build(),
                    O::Movltz => il.cmp_slt(4, self.ar(2), self.c(0)).build(),
                    _ => il.cmp_sge(4, self.ar(2), self.c(0)).build(),
                };
                self.cond_set(cond, self.a(0), self.ar(1));
            }
            O::Movt | O::Movf => {
                let flag = il.flag(self.b(2));
                let cond = if insn.opcode() == O::Movt {
                    flag
                } else {
                    il.not(0, flag).build()
                };
                self.cond_set(cond, self.a(0), self.ar(1));
            }

            // Shifts
            O::Slli => {
                let s = self.imm(2);
                if s >= 32 {
                    self.set(self.a(0), self.c(0));
                } else {
                    self.set(self.a(0), il.lsl(4, self.ar(1), self.c(s as i64)).build());
                }
            }
            O::Srli => self.set(
                self.a(0),
                il.lsr(4, self.ar(1), self.c(self.imm(2) as i64)).build(),
            ),
            O::Srai => self.set(
                self.a(0),
                il.asr(4, self.ar(1), self.c(self.imm(2) as i64)).build(),
            ),
            O::Extui => {
                let (shift, bits) = (self.imm(2) as i64, self.imm(3) as i64);
                let shifted = if shift == 0 {
                    self.ar(1)
                } else {
                    il.lsr(4, self.ar(1), self.c(shift)).build()
                };
                self.set(
                    self.a(0),
                    il.and(4, shifted, self.c((1 << bits) - 1)).build(),
                );
            }
            O::Ssl | O::Ssr | O::Ssai | O::Ssa8l | O::Ssa8b => {
                let Some(sar) = self.sr(sr::SAR) else {
                    return unimplemented(il);
                };
                let value = match insn.opcode() {
                    O::Ssai => self.c(self.imm(0) as i64),
                    O::Ssr => il.and(4, self.ar(0), self.c(31)).build(),
                    O::Ssl => il
                        .sub(4, self.c(32), il.and(4, self.ar(0), self.c(31)))
                        .build(),
                    O::Ssa8l => il
                        .lsl(4, il.and(4, self.ar(0), self.c(3)), self.c(3))
                        .build(),
                    _ => il
                        .sub(
                            4,
                            self.c(32),
                            il.lsl(4, il.and(4, self.ar(0), self.c(3)), self.c(3)),
                        )
                        .build(),
                };
                self.set(sar, value);
            }
            O::Sll | O::Srl | O::Sra | O::Src => {
                let Some(sar) = self.sr(sr::SAR) else {
                    return unimplemented(il);
                };
                let value = match insn.opcode() {
                    O::Sll => il
                        .lsl(4, self.ar(1), il.sub(4, self.c(32), self.reg(sar)))
                        .build(),
                    O::Srl => il.lsr(4, self.ar(1), self.reg(sar)).build(),
                    O::Sra => il.asr(4, self.ar(1), self.reg(sar)).build(),
                    // `src ar, as, as` is a rotate.
                    _ if ops[1] == ops[2] => il.ror(4, self.ar(1), self.reg(sar)).build(),
                    _ => {
                        let hi = il.lsl(8, il.zx(8, self.ar(1)), il.const_int(1, 32));
                        let pair = il.or(8, hi, il.zx(8, self.ar(2)));
                        il.low_part(4, il.lsr(8, pair, self.reg(sar))).build()
                    }
                };
                self.set(self.a(0), value);
            }

            // Loads and stores
            O::L8ui => self.load(1, false, self.a(0), self.mem(1, self.imm(2) as i64)),
            O::L16ui => self.load(2, false, self.a(0), self.mem(1, self.imm(2) as i64)),
            O::L16si => self.load(2, true, self.a(0), self.mem(1, self.imm(2) as i64)),
            O::L32i | O::L32iN | O::L32ai | O::L32e => {
                self.load(4, false, self.a(0), self.mem(1, self.imm(2) as i64))
            }
            O::L32r => {
                let Operand::Literal(lit) = ops[1] else {
                    unreachable!()
                };
                self.load(4, false, self.a(0), il.const_ptr(lit as u64));
            }
            O::S8i => self.store(1, self.mem(1, self.imm(2) as i64), self.ar(0)),
            O::S16i => self.store(2, self.mem(1, self.imm(2) as i64), self.ar(0)),
            O::S32i | O::S32iN | O::S32ri | O::S32e | O::S32nb => {
                self.store(4, self.mem(1, self.imm(2) as i64), self.ar(0))
            }
            O::S32c1i => {
                // Store if memory equals SCOMPARE1; at receives the old value.
                let Some(scompare1) = self.sr(sr::SCOMPARE1) else {
                    return unimplemented(il);
                };
                const OLD: u32 = 0;
                const ADDR: u32 = 1;
                self.set_temp(ADDR, 4, self.mem(1, self.imm(2) as i64));
                self.set_temp(OLD, 4, il.load(4, self.get_temp(ADDR, 4)).build());
                let mut t = LowLevelILLabel::new();
                let mut f = LowLevelILLabel::new();
                let cond = il.cmp_e(4, self.get_temp(OLD, 4), self.reg(scompare1));
                il.if_expr(cond, &mut t, &mut f).append();
                il.mark_label(&mut t);
                il.store(4, self.get_temp(ADDR, 4), self.ar(0)).append();
                il.goto(&mut f).append();
                il.mark_label(&mut f);
                self.set(self.a(0), self.get_temp(OLD, 4));
            }

            // Branches
            O::Beq | O::Bne | O::Blt | O::Bge | O::Bltu | O::Bgeu => {
                let (l, r) = (self.ar(0), self.ar(1));
                let cond = match insn.opcode() {
                    O::Beq => il.cmp_e(4, l, r).build(),
                    O::Bne => il.cmp_ne(4, l, r).build(),
                    O::Blt => il.cmp_slt(4, l, r).build(),
                    O::Bge => il.cmp_sge(4, l, r).build(),
                    O::Bltu => il.cmp_ult(4, l, r).build(),
                    _ => il.cmp_uge(4, l, r).build(),
                };
                self.branch(cond, self.target());
            }
            O::Beqi | O::Bnei | O::Blti | O::Bgei | O::Bltui | O::Bgeui => {
                let (l, r) = (self.ar(0), self.c(self.imm(1) as i64));
                let cond = match insn.opcode() {
                    O::Beqi => il.cmp_e(4, l, r).build(),
                    O::Bnei => il.cmp_ne(4, l, r).build(),
                    O::Blti => il.cmp_slt(4, l, r).build(),
                    O::Bgei => il.cmp_sge(4, l, r).build(),
                    O::Bltui => il.cmp_ult(4, l, r).build(),
                    _ => il.cmp_uge(4, l, r).build(),
                };
                self.branch(cond, self.target());
            }
            O::Beqz | O::BeqzN | O::Bnez | O::BnezN | O::Bltz | O::Bgez => {
                let (l, r) = (self.ar(0), self.c(0));
                let cond = match insn.opcode() {
                    O::Beqz | O::BeqzN => il.cmp_e(4, l, r).build(),
                    O::Bnez | O::BnezN => il.cmp_ne(4, l, r).build(),
                    O::Bltz => il.cmp_slt(4, l, r).build(),
                    _ => il.cmp_sge(4, l, r).build(),
                };
                self.branch(cond, self.target());
            }
            O::Ball | O::Bnall => {
                let masked = il.and(4, il.not(4, self.ar(0)), self.ar(1));
                let cond = if insn.opcode() == O::Ball {
                    il.cmp_e(4, masked, self.c(0)).build()
                } else {
                    il.cmp_ne(4, masked, self.c(0)).build()
                };
                self.branch(cond, self.target());
            }
            O::Bany | O::Bnone => {
                let masked = il.and(4, self.ar(0), self.ar(1));
                let cond = if insn.opcode() == O::Bany {
                    il.cmp_ne(4, masked, self.c(0)).build()
                } else {
                    il.cmp_e(4, masked, self.c(0)).build()
                };
                self.branch(cond, self.target());
            }
            O::Bbc | O::Bbs => {
                let bit = il.and(4, self.ar(1), self.c(31));
                let masked = il.and(4, il.lsr(4, self.ar(0), bit), self.c(1));
                let cond = if insn.opcode() == O::Bbc {
                    il.cmp_e(4, masked, self.c(0)).build()
                } else {
                    il.cmp_ne(4, masked, self.c(0)).build()
                };
                self.branch(cond, self.target());
            }
            O::Bbci | O::Bbsi => {
                let masked = il.and(4, self.ar(0), self.c(1i64 << self.imm(1)));
                let cond = if insn.opcode() == O::Bbci {
                    il.cmp_e(4, masked, self.c(0)).build()
                } else {
                    il.cmp_ne(4, masked, self.c(0)).build()
                };
                self.branch(cond, self.target());
            }
            O::Bt | O::Bf => {
                let flag = il.flag(self.b(0));
                let cond = if insn.opcode() == O::Bt {
                    flag
                } else {
                    il.not(0, flag).build()
                };
                self.branch(cond, self.target());
            }

            // Jumps, calls and returns
            O::J => self.goto_addr(self.target()),
            O::Jx => il.jump(self.ar(0)).append(),
            O::Call0 => il.call(il.const_ptr(self.target())).append(),
            O::Callx0 => il.call(self.ar(0)).append(),
            O::Call4 => self.windowed_call(4, il.const_ptr(self.target())),
            O::Call8 => self.windowed_call(8, il.const_ptr(self.target())),
            O::Call12 => self.windowed_call(12, il.const_ptr(self.target())),
            O::Callx4 => self.windowed_call(4, self.ar(0)),
            O::Callx8 => self.windowed_call(8, self.ar(0)),
            O::Callx12 => self.windowed_call(12, self.ar(0)),
            O::Ret | O::RetN => il.ret(self.reg(Reg::A(0))).append(),
            O::Retw | O::RetwN => {
                // What the caller of a `call8` sees: the result arrives in its
                // a10/a11, which are our a2/a3 (see cc.rs).
                self.set(Reg::A(10), self.reg(Reg::A(2)));
                self.set(Reg::A(11), self.reg(Reg::A(3)));
                il.ret(self.reg(Reg::A(0))).append();
            }
            O::Entry => {
                // The windowed calling convention names the arguments the way
                // the `call8` caller sees them, a10..a15 (see cc.rs). After
                // the window rotation, the callee finds them in a2..a7.
                for k in 2..=7 {
                    self.set(Reg::A(k), self.reg(Reg::A(k + 8)));
                }
                let sp = self.a(0);
                self.set(
                    sp,
                    il.sub(4, self.reg(sp), self.c(self.imm(1) as i64)).build(),
                );
            }

            // Zero-overhead loops
            O::Loop | O::Loopnez | O::Loopgtz => {
                let end = self.target();
                if let Some(loops) = self.loops {
                    loops.record(self.next(), end);
                }
                let (Some(lbeg), Some(lend), Some(lcount)) =
                    (self.sr(sr::LBEG), self.sr(sr::LEND), self.sr(sr::LCOUNT))
                else {
                    return unimplemented(il);
                };
                self.set(lcount, il.sub(4, self.ar(0), self.c(1)).build());
                self.set(lbeg, il.const_ptr(self.next()));
                self.set(lend, il.const_ptr(end));
                match insn.opcode() {
                    O::Loopnez => self.branch(il.cmp_e(4, self.ar(0), self.c(0)).build(), end),
                    O::Loopgtz => self.branch(il.cmp_sle(4, self.ar(0), self.c(0)).build(), end),
                    _ => {}
                }
            }

            // Special and user registers
            O::Rsr | O::Rur | O::Wsr | O::Wur | O::Xsr
                if operand_reg(self.isa, &ops[1]).is_some_and(|r| r.is_system()) =>
            {
                self.system_register()
            }
            O::Rsr | O::Rur => {
                let r = operand_reg(self.isa, &ops[1]).unwrap();
                if matches!(r, Reg::Sr(s) if s.number == sr::BR) {
                    self.set(self.a(0), self.read_br());
                } else {
                    self.set(self.a(0), self.reg(r));
                }
            }
            O::Wsr | O::Wur => {
                let r = operand_reg(self.isa, &ops[1]).unwrap();
                if matches!(r, Reg::Sr(s) if s.number == sr::BR) {
                    self.write_br(self.ar(0));
                } else {
                    self.set(r, self.ar(0));
                }
            }
            O::Xsr => {
                let r = operand_reg(self.isa, &ops[1]).unwrap();
                const OLD: u32 = 0;
                if matches!(r, Reg::Sr(s) if s.number == sr::BR) {
                    self.set_temp(OLD, 4, self.read_br());
                    self.write_br(self.ar(0));
                } else {
                    self.set_temp(OLD, 4, self.reg(r));
                    self.set(r, self.ar(0));
                }
                self.set(self.a(0), self.get_temp(OLD, 4));
            }
            // Exceptions and traps
            O::Syscall => il.syscall().append(),
            O::Break | O::BreakN => il.bp().append(),
            O::Ill | O::IllN => il.trap(0).append(),
            O::Rfe | O::Rfwo | O::Rfwu => {
                self.exception_return(special_register_by_name(self.isa, "epc1").map(Reg::Sr))
            }
            O::Rfde => {
                self.exception_return(special_register_by_name(self.isa, "depc").map(Reg::Sr))
            }
            O::Rfi => {
                let name = format!("epc{}", self.imm(0));
                self.exception_return(special_register_by_name(self.isa, &name).map(Reg::Sr))
            }
            O::Rfdo | O::Rfdd => self.exception_return(None),

            // Boolean registers
            O::Andb | O::Andbc | O::Orb | O::Orbc | O::Xorb => {
                let s = il.flag(self.b(1));
                let t = il.flag(self.b(2));
                let t = match insn.opcode() {
                    O::Andbc | O::Orbc => il.not(0, t).build(),
                    _ => t,
                };
                let value = match insn.opcode() {
                    O::Andb | O::Andbc => il.and(0, s, t).build(),
                    O::Orb | O::Orbc => il.or(0, s, t).build(),
                    _ => il.xor(0, s, t).build(),
                };
                il.set_flag(self.b(0), value).append();
            }
            O::Any4 | O::All4 | O::Any8 | O::All8 => {
                let Operand::BrGroup { first, count } = ops[1] else {
                    unreachable!()
                };
                let any = matches!(insn.opcode(), O::Any4 | O::Any8);
                let mut value = il.flag(Flag(first));
                for i in 1..count {
                    let next = il.flag(Flag(first + i));
                    value = if any {
                        il.or(0, value, next).build()
                    } else {
                        il.and(0, value, next).build()
                    };
                }
                il.set_flag(self.b(0), value).append();
            }

            // Floating point
            O::AddS => self.set(self.f(0), il.fadd(4, self.fr(1), self.fr(2)).build()),
            O::SubS => self.set(self.f(0), il.fsub(4, self.fr(1), self.fr(2)).build()),
            O::MulS => self.set(self.f(0), il.fmul(4, self.fr(1), self.fr(2)).build()),
            O::MaddS | O::MaddnS | O::MsubS => {
                // Fused: the product of two singles is exact as a double, so
                // computing in double precision rounds the product only
                // together with the accumulation.
                let double = |e| il.float_conv(8, e).build();
                let product = il.fmul(8, double(self.fr(1)), double(self.fr(2)));
                let acc = double(self.fr(0));
                let value = if insn.opcode() == O::MsubS {
                    il.fsub(8, acc, product).build()
                } else {
                    il.fadd(8, acc, product).build()
                };
                self.set(self.f(0), il.float_conv(4, value).build());
            }
            O::NegS => self.set(self.f(0), il.fneg(4, self.fr(1)).build()),
            O::AbsS => self.set(self.f(0), il.fabs(4, self.fr(1)).build()),
            O::MovS => self.set(self.f(0), self.fr(1)),
            O::Rfr => self.set(self.a(0), self.fr(1)),
            O::Wfr => self.set(self.f(0), self.ar(1)),
            O::FloatS | O::UfloatS => {
                let int = if insn.opcode() == O::FloatS {
                    self.ar(1)
                } else {
                    il.zx(8, self.ar(1)).build()
                };
                let value = il.int_to_float(4, int).build();
                let value = match self.imm(2) {
                    0 => value,
                    s => il.fdiv(4, value, self.scale(s)).build(),
                };
                self.set(self.f(0), value);
            }
            O::TruncS | O::UtruncS | O::RoundS | O::FloorS | O::CeilS => {
                let value = match self.imm(2) {
                    0 => self.fr(1),
                    s => il.fmul(4, self.fr(1), self.scale(s)).build(),
                };
                let value = match insn.opcode() {
                    O::RoundS => il.round_to_int(4, value).build(),
                    O::FloorS => il.floor(4, value).build(),
                    O::CeilS => il.ceil(4, value).build(),
                    _ => il.ftrunc(4, value).build(),
                };
                self.set(self.a(0), il.float_to_int(4, value).build());
            }
            O::OeqS | O::OltS | O::OleS | O::UnS | O::UeqS | O::UltS | O::UleS => {
                let (l, r) = (self.fr(1), self.fr(2));
                let value = match insn.opcode() {
                    O::OeqS => il.fcmp_e(4, l, r).build(),
                    O::OltS => il.fcmp_lt(4, l, r).build(),
                    O::OleS => il.fcmp_le(4, l, r).build(),
                    O::UnS => il.fcmp_uo(4, l, r).build(),
                    _ => {
                        let unordered = il.fcmp_uo(4, self.fr(1), self.fr(2));
                        let ordered = match insn.opcode() {
                            O::UeqS => il.fcmp_e(4, l, r).build(),
                            O::UltS => il.fcmp_lt(4, l, r).build(),
                            _ => il.fcmp_le(4, l, r).build(),
                        };
                        il.or(0, unordered, ordered).build()
                    }
                };
                il.set_flag(self.b(0), value).append();
            }
            O::MoveqzS | O::MovnezS | O::MovltzS | O::MovgezS => {
                let cond = match insn.opcode() {
                    O::MoveqzS => il.cmp_e(4, self.ar(2), self.c(0)).build(),
                    O::MovnezS => il.cmp_ne(4, self.ar(2), self.c(0)).build(),
                    O::MovltzS => il.cmp_slt(4, self.ar(2), self.c(0)).build(),
                    _ => il.cmp_sge(4, self.ar(2), self.c(0)).build(),
                };
                self.cond_set(cond, self.f(0), self.fr(1));
            }
            O::MovtS | O::MovfS => {
                let flag = il.flag(self.b(2));
                let cond = if insn.opcode() == O::MovtS {
                    flag
                } else {
                    il.not(0, flag).build()
                };
                self.cond_set(cond, self.f(0), self.fr(1));
            }
            O::Lsi => self.load(4, false, self.f(0), self.mem(1, self.imm(2) as i64)),
            O::Lsx => self.load(
                4,
                false,
                self.f(0),
                il.add(4, self.ar(1), self.ar(2)).build(),
            ),
            O::Ssi => self.store(4, self.mem(1, self.imm(2) as i64), self.fr(0)),
            O::Ssx => self.store(4, il.add(4, self.ar(1), self.ar(2)).build(), self.fr(0)),
            // Post-increment forms: access at `as`, then advance it.
            O::Lsip | O::Lsxp | O::Ssip | O::Ssxp => {
                match insn.opcode() {
                    O::Lsip | O::Lsxp => self.load(4, false, self.f(0), self.ar(1)),
                    _ => self.store(4, self.ar(1), self.fr(0)),
                }
                let inc = match insn.opcode() {
                    O::Lsip | O::Ssip => self.c(self.imm(2) as i64),
                    _ => self.ar(2),
                };
                self.set(self.a(1), il.add(4, self.ar(1), inc).build());
            }

            // MAC16 loads with address update; the multiply part of the
            // combined forms is an intrinsic.
            O::Ldinc | O::Lddec => self.mac16_load(),
            op if op.mnemonic().ends_with(".ldinc") || op.mnemonic().ends_with(".lddec") => {
                self.intrinsic();
                self.mac16_load();
            }

            // ESP32-S3 vector loads, stores and moves with simple semantics.
            O::LdQr => {
                let Operand::Qr(q) = ops[0] else {
                    unreachable!()
                };
                self.load(16, false, Reg::Q(q), self.mem(1, self.imm(2) as i64));
            }
            O::StQr => {
                let Operand::Qr(q) = ops[0] else {
                    unreachable!()
                };
                self.store(16, self.mem(1, self.imm(2) as i64), self.reg(Reg::Q(q)));
            }
            O::MvQr => {
                let (Operand::Qr(d), Operand::Qr(s)) = (ops[0], ops[1]) else {
                    unreachable!()
                };
                self.set(Reg::Q(d), self.reg(Reg::Q(s)));
            }
            O::EeZeroQ => {
                let Operand::Qr(q) = ops[0] else {
                    unreachable!()
                };
                self.set(Reg::Q(q), il.const_int(16, 0));
            }
            O::EeAndq | O::EeOrq | O::EeXorq | O::EeNotq => {
                let q = |i: usize| match ops[i] {
                    Operand::Qr(n) => Reg::Q(n),
                    _ => unreachable!(),
                };
                let x = self.reg(q(1));
                let value = match insn.opcode() {
                    O::EeAndq => il.and(16, x, self.reg(q(2))).build(),
                    O::EeOrq => il.or(16, x, self.reg(q(2))).build(),
                    O::EeXorq => il.xor(16, x, self.reg(q(2))).build(),
                    _ => il.not(16, x).build(),
                };
                self.set(q(0), value);
            }
            O::EeVld128Ip | O::EeVld128Xp | O::EeVst128Ip | O::EeVst128Xp => {
                let Operand::Qr(q) = ops[0] else {
                    unreachable!()
                };
                // The address is 16-byte aligned by ignoring its low bits.
                let addr = il.and(4, self.ar(1), self.c(!0xf)).build();
                match insn.opcode() {
                    O::EeVld128Ip | O::EeVld128Xp => self.load(16, false, Reg::Q(q), addr),
                    _ => self.store(16, addr, self.reg(Reg::Q(q))),
                }
                let inc = match ops[2] {
                    Operand::Imm(v) => self.c(v as i64),
                    _ => self.ar(2),
                };
                self.set(self.a(1), il.add(4, self.ar(1), inc).build());
            }

            op => {
                debug_assert!(uses_intrinsic(op), "{op:?} has native semantics");
                self.intrinsic()
            }
        }

        if insn.control_flow() == xtensa::ControlFlow::Sequential
            || matches!(
                insn.control_flow(),
                xtensa::ControlFlow::Loop {
                    conditional: false,
                    ..
                }
            )
        {
            self.loop_check();
        }
    }

    /// Accesses to special and user registers that hold machine state
    /// (interrupt, exception, timer, cache and debug control, ...) are
    /// intrinsics: they have side effects and their values change outside
    /// the program's control, so modelling them as plain registers would let
    /// the decompiler drop writes and present reads as uninitialized
    /// variables.
    fn system_register(&self) {
        let reg = operand_reg(self.isa, &self.ops()[1]).unwrap();
        let access = match self.insn.opcode() {
            O::Rsr | O::Rur => SysAccess::Read,
            O::Wsr | O::Wur => SysAccess::Write,
            _ => SysAccess::Exchange,
        };
        let Some(&intrinsic) =
            intrinsics::sys_id(reg, access).and_then(|id| self.intrinsics.get(&id))
        else {
            return unimplemented(self.il);
        };
        let outputs: Vec<Reg> = if access == SysAccess::Write {
            vec![]
        } else {
            vec![self.a(0)]
        };
        let inputs: Vec<Expr<'a>> = if access == SysAccess::Read {
            vec![]
        } else {
            vec![self.ar(0)]
        };
        self.il.intrinsic(outputs, intrinsic, inputs).append();
    }

    /// `ldinc mw, as`: `as += 4; mw = [as]`; `lddec`: `as -= 4; mw = [as]`.
    /// The combined MAC16 forms have the same first two operands.
    fn mac16_load(&self) {
        let il = self.il;
        let Some(m) = operand_reg(self.isa, &self.ops()[0]) else {
            return unimplemented(il);
        };
        let delta = if self.insn.opcode().mnemonic().ends_with("ldinc") {
            4
        } else {
            -4
        };
        self.set(self.a(1), il.add(4, self.ar(1), self.c(delta)).build());
        self.load(4, false, m, self.ar(1));
    }

    /// The `br` special register, assembled from the sixteen boolean flags.
    fn read_br(&self) -> Expr<'a> {
        let il = self.il;
        let mut value = il.bool_to_int(4, il.flag(Flag(0))).build();
        for i in 1..16u8 {
            let bit = il.lsl(4, il.bool_to_int(4, il.flag(Flag(i))), self.c(i as i64));
            value = il.or(4, value, bit).build();
        }
        value
    }

    fn write_br(&self, value: Expr<'a>) {
        let il = self.il;
        const VALUE: u32 = 0;
        self.set_temp(VALUE, 4, value);
        for i in 0..16u8 {
            let bit = il.and(4, self.get_temp(VALUE, 4), self.c(1 << i));
            il.set_flag(Flag(i), il.cmp_ne(4, bit, self.c(0))).append();
        }
    }
}
