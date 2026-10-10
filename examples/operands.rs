//! Prints the decoded operands of every exemplar, for checking lifter assumptions.
use binja_xtensa::xtensa::exemplars::EXEMPLARS;
use xtensa_isa::Isa;

fn main() {
    let mut seen = std::collections::BTreeMap::new();
    for isa in [Isa::ESP8266, Isa::ESP32, Isa::ESP32S2, Isa::ESP32S3] {
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
