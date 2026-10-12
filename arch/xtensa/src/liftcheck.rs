//! Developer checks of the lifter, built only with the `liftcheck` feature.
//! They are unit tests, so that they work with the plugin built as a
//! `cdylib` alone, as Binary Ninja's own architecture plugins are (whose
//! integration tests and examples could not link it):
//!
//! ```sh
//! # Lifts ~1.5M encodings through the real core (needs a headless-capable
//! # Binary Ninja license).
//! cargo test --release -p arch_xtensa --features liftcheck --lib liftcheck::lift_sweep
//! # Prints the decoded operands of every opcode, for checking what the
//! # lifter assumes about them; needs no Binary Ninja.
//! cargo test -p arch_xtensa --no-default-features --features liftcheck --lib liftcheck::operands -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::Ordering;

use binaryninja::architecture::{Architecture, CoreArchitecture};
use binaryninja::headless::Session;
use binaryninja::low_level_il::LowLevelILMutableFunction;
use binaryninja::rc::Ref;

use crate::Core;
use crate::exemplars::EXEMPLARS;
use crate::lift::UNIMPLEMENTED;

/// The encodings the sweep lifts on `core`: every exemplar, every 2-byte
/// word, a sample of the 3-byte words and, on the ESP32-S3, of the 4-byte
/// words.
fn encodings(core: Core) -> Vec<(u32, usize)> {
    let mut words: Vec<(u32, usize)> = EXEMPLARS.iter().map(|&(w, l)| (w, l as usize)).collect();
    words.extend((0..0x1_0000u32).map(|w| (w, 2)));
    words.extend((0..0x100_0000u32).step_by(17).map(|w| (w, 3)));
    if core == Core::Esp32S3 {
        words.extend((0..u32::MAX).step_by(4099).map(|w| (w, 4)));
    }
    words
}

/// Lifts a broad sample of every core's instruction encodings through the
/// real Binary Ninja core and checks that nothing panics or falls back to
/// `unimplemented`.
#[test]
fn lift_sweep() {
    let _session = Session::new().expect(
        "the lift sweep needs Binary Ninja to initialize headlessly (the `core` feature and a headless-capable license)",
    );
    crate::register();

    for core in Core::ALL {
        let arch = CoreArchitecture::by_name(core.arch_name()).expect("architecture registered");
        let isa = core.isa();
        let mut il: Option<Ref<LowLevelILMutableFunction>> = None;
        let (mut lifted, mut seen) = (0usize, HashSet::new());
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
            let before = UNIMPLEMENTED.load(Ordering::Relaxed);
            let result = arch.instruction_llil(&bytes[..len], 0x4008_0000, il);
            assert_eq!(
                result.map(|r| r.0),
                Some(len),
                "{} {insn}",
                core.arch_name()
            );
            assert_eq!(
                UNIMPLEMENTED.load(Ordering::Relaxed),
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

/// Prints the decoded operands of every exemplar (one per opcode), for
/// checking what the lifter assumes about them.
#[test]
#[ignore = "prints a listing; run with --ignored --nocapture"]
fn operands() {
    let mut seen = BTreeMap::new();
    for core in Core::ALL {
        let isa = core.isa();
        for &(word, len) in EXEMPLARS {
            let bytes = word.to_le_bytes();
            if let Ok(i) = isa.decode(&bytes[..len as usize], 0x1000) {
                seen.entry(i.opcode())
                    .or_insert_with(|| format!("{:<28} {:?}", i.to_string(), i.operands()));
            }
        }
    }
    for (_, s) in seen {
        println!("{s}");
    }
}
