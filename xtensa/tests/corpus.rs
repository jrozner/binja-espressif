//! Decodes real compiled code and compares it with GNU objdump.
//!
//! `tests/data/corpus` is produced by `scripts/oracle/build_corpus.sh`, which
//! compiles `tests/corpus/*.c` with Espressif's GCC for each chip and records
//! the raw `.text` bytes alongside objdump's listing of them.

mod common;

use std::fs;
use std::path::Path;

use common::objdump::{Line, compare_bytes, isa_by_name};

/// Flash-mapped instruction addresses the corpus is linked at.
fn text_base(isa: &str) -> u32 {
    match isa {
        "esp32" => 0x400d_0000,
        "esp32s2" => 0x4008_0000,
        "esp32s3" => 0x4200_0000,
        "esp8266" => 0x4020_1000,
        _ => unreachable!(),
    }
}

#[test]
fn corpus_matches_objdump() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/corpus");
    let mut listings: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tsv"))
        .collect();
    listings.sort();
    assert!(
        listings.len() >= 28,
        "corpus is missing; run scripts/oracle/build_corpus.sh"
    );

    let (mut checked, mut failures) = (0, Vec::new());
    for listing in &listings {
        let stem = listing.file_stem().unwrap().to_str().unwrap();
        let isa_name = stem.split('-').next().unwrap();
        let isa = isa_by_name(isa_name).unwrap();
        let base = text_base(isa_name);
        let code = fs::read(listing.with_extension("bin")).unwrap();
        let text = fs::read_to_string(listing).unwrap();
        let lines: Vec<Line<'_>> = text.lines().map(Line::parse).collect();

        for (i, line) in lines.iter().enumerate() {
            // Alignment padding between functions.
            if line.text.starts_with(".byte") {
                continue;
            }
            let offset = (line.address - base) as usize;
            let bytes = &code[offset..];
            let word = bytes[..line.len]
                .iter()
                .rev()
                .fold(0, |w, &b| w << 8 | u32::from(b));
            assert_eq!(
                word, line.word,
                "{stem}: listing does not match binary at {:x}",
                line.address
            );
            checked += 1;
            if let Err(diff) = compare_bytes(&isa, bytes, line) {
                failures.push(format!("{stem}: {diff}"));
            }
            // Linear disassembly must land on objdump's next instruction,
            // possibly after a few bytes of zero alignment padding, which
            // objdump does not always list.
            if let (Some(next), Ok(insn)) = (lines.get(i + 1), isa.decode(bytes, line.address)) {
                let gap = next.address.wrapping_sub(insn.next_address()) as usize;
                let padding = &bytes[line.len..line.len + gap.min(4)];
                assert!(
                    gap < 4 && padding.iter().all(|&b| b == 0),
                    "{stem}: desynchronized after {:x}",
                    line.address
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} instructions differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(checked > 2000, "only {checked} instructions checked");
}

/// Disassembling a whole function linearly yields no invalid instructions.
#[test]
fn linear_sweep_of_function() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/corpus");
    let code = fs::read(dir.join("esp32-integer-O2.bin")).unwrap();
    let listing = fs::read_to_string(dir.join("esp32-integer-O2.tsv")).unwrap();
    let first = Line::parse(listing.lines().next().unwrap());
    let start = (first.address - text_base("esp32")) as usize;
    let isa = isa_by_name("esp32").unwrap();
    let decoded: Vec<_> = isa
        .disassemble(&code[start..], first.address)
        .take_while(|(_, insn)| insn.is_ok())
        .map(|(_, insn)| insn.unwrap())
        .collect();
    assert!(decoded.len() > 10);
    assert_eq!(decoded[0].opcode(), xtensa::Opcode::Entry);
    assert!(
        decoded
            .iter()
            .any(|insn| insn.opcode() == xtensa::Opcode::RetwN)
    );
}
