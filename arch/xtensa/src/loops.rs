//! Zero-overhead loops.
//!
//! `loop as, end` (and `loopnez`/`loopgtz`) sets `LBEG` to the next
//! instruction, `LEND` to `end` and `LCOUNT` to `as - 1`. From then on,
//! whenever execution falls through to `LEND` while `LCOUNT` is non-zero, the
//! processor decrements `LCOUNT` and continues at `LBEG` instead. The branch
//! back is therefore a property of the *last instruction of the loop body*,
//! which on its own looks like any other instruction.
//!
//! Binary Ninja's per-instruction callbacks only see the bytes of one
//! instruction. The intended mechanism for this kind of state is the
//! function architecture context filled in by an `analyze_basic_blocks`
//! override, but the Rust bindings offer no way for an override to fall back
//! to the core's default block analysis and lifting (see LIMITATIONS.md), and
//! reimplementing those is out of proportion to the problem.
//!
//! Instead, decoding a loop instruction records `LEND -> LBEG` in a table
//! owned by the architecture, so each chip has its own table and cores
//! without the loop option (the ESP8266) never see a loop edge. The loop
//! instruction always precedes its body in the core's block analysis and
//! lifting, so by the time the last body instruction is examined the entry
//! exists. An instruction is only treated as a loop end if it lies inside
//! the recorded body (`LBEG <= addr` and it ends exactly at `LEND`); loop
//! bodies are at most 256 bytes. The table is still shared between all open
//! binaries for the same chip (see LIMITATIONS.md).

use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Default)]
pub struct Loops(RwLock<HashMap<u64, u64>>);

impl Loops {
    /// Records a loop whose body spans `lbeg..lend`.
    pub fn record(&self, lbeg: u64, lend: u64) {
        if lend <= lbeg {
            return;
        }
        if self.0.read().unwrap().get(&lend) == Some(&lbeg) {
            return;
        }
        self.0.write().unwrap().insert(lend, lbeg);
    }

    /// If the instruction at `addr` ending at `next` is the last one of a
    /// loop body, returns the loop's `LBEG`.
    pub fn loop_back(&self, addr: u64, next: u64) -> Option<u64> {
        let lbeg = *self.0.read().unwrap().get(&next)?;
        (lbeg <= addr).then_some(lbeg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_independent() {
        let (a, b) = (Loops::default(), Loops::default());
        a.record(0x4008_0003, 0x4008_0024);
        assert_eq!(a.loop_back(0x4008_0021, 0x4008_0024), Some(0x4008_0003));
        assert_eq!(a.loop_back(0x4008_0000, 0x4008_0024), None);
        assert_eq!(b.loop_back(0x4008_0021, 0x4008_0024), None);
    }
}
