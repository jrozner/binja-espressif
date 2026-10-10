//! Lifts a broad sample of every core's instruction encodings through the
//! real Binary Ninja core and checks that nothing panics or falls back to
//! `unimplemented`.
//!
//! Needs a headless-capable Binary Ninja installation and license; the test
//! is skipped (with a message) when the core cannot be initialized.

use binaryninja::architecture::{Architecture, CoreArchitecture};
use binaryninja::headless::Session;
use binaryninja::low_level_il::LowLevelILMutableFunction;
use binja_xtensa::xtensa::Core;
use binja_xtensa::xtensa::exemplars::EXEMPLARS;
use std::sync::atomic::Ordering;

fn encodings(core: Core) -> Vec<(u32, usize)> {
    let mut words: Vec<(u32, usize)> = EXEMPLARS.iter().map(|&(w, l)| (w, l as usize)).collect();
    words.extend((0..0x1_0000u32).map(|w| (w, 2)));
    words.extend((0..0x100_0000u32).step_by(17).map(|w| (w, 3)));
    if core == Core::Esp32S3 {
        words.extend((0..u32::MAX).step_by(4099).map(|w| (w, 4)));
    }
    words
}

#[test]
fn lift_sweep() {
    let Ok(_session) = Session::new() else {
        eprintln!("skipping: Binary Ninja could not be initialized headlessly");
        return;
    };
    assert!(binja_xtensa::CorePluginInit());

    for core in Core::ALL {
        let arch = CoreArchitecture::by_name(core.arch_name()).expect("architecture registered");
        let isa = core.isa();
        let mut il: Option<binaryninja::rc::Ref<LowLevelILMutableFunction>> = None;
        let (mut lifted, mut seen) = (0usize, std::collections::HashSet::new());
        for (i, (word, len)) in encodings(core).into_iter().enumerate() {
            let bytes = word.to_le_bytes();
            let Ok(insn) = isa.decode(&bytes[..len], 0x4008_0000) else {
                continue;
            };
            if insn.len() != len {
                continue;
            }
            if i % 5000 == 0 || il.is_none() {
                il = Some(LowLevelILMutableFunction::new(arch, None));
            }
            let il = il.as_ref().unwrap();
            let before = binja_xtensa::xtensa::lift::UNIMPLEMENTED.load(Ordering::Relaxed);
            let result = arch.instruction_llil(&bytes[..len], 0x4008_0000, il);
            assert_eq!(
                result.map(|r| r.0),
                Some(len),
                "{} {insn}",
                core.arch_name()
            );
            assert_eq!(
                binja_xtensa::xtensa::lift::UNIMPLEMENTED.load(Ordering::Relaxed),
                before,
                "{}: `{insn}` ({word:#x}) lifted as unimplemented",
                core.arch_name()
            );
            lifted += 1;
            seen.insert(insn.opcode());
        }
        eprintln!(
            "{}: lifted {lifted} instructions, {} opcodes",
            core.arch_name(),
            seen.len()
        );
    }
}
