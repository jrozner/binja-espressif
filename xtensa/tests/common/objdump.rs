//! Comparison of decoder output with GNU objdump, shared by the fixture tests
//! and `examples/oracle_diff.rs`.

use xtensa::{Isa, Operand};

/// One line of `scripts/oracle/sweep.py` output.
pub struct Line<'a> {
    pub word: u32,
    pub address: u32,
    pub len: usize,
    pub text: &'a str,
}

impl<'a> Line<'a> {
    /// Parses `<hex word>\t<hex address>\t<length>\t<objdump text>`.
    pub fn parse(line: &'a str) -> Line<'a> {
        let mut fields = line.splitn(4, '\t');
        let mut next = || {
            fields
                .next()
                .unwrap_or_else(|| panic!("malformed line: {line}"))
        };
        let word = u32::from_str_radix(next(), 16).expect("word");
        let address = u32::from_str_radix(next(), 16).expect("address");
        let len = next().parse().expect("length");
        Line {
            word,
            address,
            len,
            text: next(),
        }
    }
}

/// The supported configurations, by the names used for fixtures (which are
/// also the toolchain's dynamic configuration names).
pub fn isa_by_name(name: &str) -> Option<Isa> {
    match name {
        "esp32" => Some(Isa::ESP32),
        "esp32s2" => Some(Isa::ESP32S2),
        "esp32s3" => Some(Isa::ESP32S3),
        "esp8266" => Some(Isa::ESP8266),
        _ => None,
    }
}

/// Normalizes objdump's text into what our `Display` produces, or `None` if
/// objdump did not decode an instruction.
fn expected(word: u32, text: &str) -> Option<String> {
    let mnemonic = text.split(' ').next().unwrap_or_default();
    // objdump prints undecodable bytes as `.byte`. When libisa fails to match
    // an opcode it also falls back to the first opcode in the configuration's
    // table, `lsi` on the ESP32 and `excw` on the LX106; those are invalid
    // unless the bits really encode that instruction.
    if mnemonic == ".byte"
        || (mnemonic == "lsi" && word & 0xf00f != 0x0003)
        || (mnemonic == "excw" && word != 0x2080)
    {
        return None;
    }
    // Drop symbolic annotations such as ` <c12+0x4>`.
    let text = text.split(" <").next().unwrap_or_default();
    // The LX106 configuration does not name the CONFIGID registers.
    Some(
        text.replace(".176 ", ".configid0 ")
            .replace(".208 ", ".configid1 "),
    )
}

/// Decodes `line.word` and compares it with objdump, returning a description
/// of the difference, if any.
pub fn compare(isa: &Isa, line: &Line<'_>) -> Result<(), String> {
    // Each sweep candidate is the four little-endian bytes of its word.
    compare_bytes(isa, &line.word.to_le_bytes(), line)
}

/// Decodes `bytes`, which start with the instruction objdump listed in
/// `line`, and compares the result with objdump.
pub fn compare_bytes(isa: &Isa, bytes: &[u8], line: &Line<'_>) -> Result<(), String> {
    let expected = expected(line.word, line.text);
    let actual = isa.decode(bytes, line.address).ok().map(|insn| {
        let mut text = insn.to_string();
        // objdump prints code and literal addresses without a 0x prefix.
        if let Some(Operand::Target(t) | Operand::Literal(t)) = insn.operands().last() {
            let suffix = format!("{t:#x}");
            text.truncate(text.len() - suffix.len());
            text.push_str(&suffix[2..]);
        }
        (insn.len(), text)
    });
    let matches = match (&expected, &actual) {
        (None, None) => true,
        // The LX106 binutils configuration omits the operands of the external
        // register instructions, although they are encoded as on the ESP32.
        (Some(e), Some((len, a))) if e == "rer" || e == "wer" => {
            *len == line.len && a.split(' ').next() == Some(e.as_str())
        }
        (Some(e), Some((len, a))) => *len == line.len && e == a,
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(format!(
            "{:06x}@{:x}: objdump={expected:?} ours={actual:?}",
            line.word, line.address
        ))
    }
}
