//! Disassembles a raw binary, e.g. a section extracted from ESP firmware.
//!
//! ```text
//! cargo run --example disasm -- esp32 firmware.text.bin 0x400d0000
//! ```

use std::fmt::Write;
use std::process::ExitCode;

use xtensa::{ControlFlow, Isa};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (isa, path, base) = match args.as_slice() {
        [isa, path] => (isa, path, "0"),
        [isa, path, base] => (isa, path, base.as_str()),
        _ => {
            eprintln!("usage: disasm <esp32|esp32s2|esp32s3|esp8266> <file> [base address]");
            return ExitCode::FAILURE;
        }
    };
    let isa = match isa.as_str() {
        "esp32" => Isa::ESP32,
        "esp32s2" => Isa::ESP32S2,
        "esp32s3" => Isa::ESP32S3,
        "esp8266" => Isa::ESP8266,
        other => {
            eprintln!("unknown ISA {other:?}");
            return ExitCode::FAILURE;
        }
    };
    let Ok(base) = u32::from_str_radix(base.trim_start_matches("0x"), 16) else {
        eprintln!("invalid base address {base:?}");
        return ExitCode::FAILURE;
    };
    let code = match std::fs::read(path) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    for (address, result) in isa.disassemble(&code, base) {
        // Addresses wrap at 2^32 (as in the library), so the offset into the
        // file does too; inputs are far smaller than 4 GiB.
        let offset = address.wrapping_sub(base) as usize;
        match result {
            Ok(insn) => {
                let mut bytes = String::new();
                for b in &code[offset..offset + insn.len()] {
                    write!(bytes, "{b:02x}").unwrap();
                }
                let note = match insn.control_flow() {
                    ControlFlow::Return | ControlFlow::Jump { .. } | ControlFlow::IndirectJump => {
                        "\n"
                    }
                    _ => "",
                };
                println!("{address:08x}:  {bytes:<8} {insn}{note}");
            }
            Err(err) => println!("{address:08x}:  {:02x}       ; {err}", code[offset]),
        }
    }
    ExitCode::SUCCESS
}
