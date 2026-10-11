//! A dependency-free, `no_std` disassembler for the Xtensa instruction set
//! as implemented by the Espressif ESP8266 (LX106), ESP32 (LX6), ESP32-S2 and
//! ESP32-S3 (LX7) cores.
//!
//! Decoding is configured by an [`Isa`], which records the optional
//! instruction groups and special registers a core implements. Encodings that
//! the selected core does not implement are rejected rather than decoded as
//! something the hardware would trap on.
//!
//! ```
//! use xtensa::{ControlFlow, Isa, Opcode, Operand};
//!
//! // entry a1, 32 ; movi.n a8, 0 ; retw.n
//! let code = [0x36, 0x41, 0x00, 0x0c, 0x08, 0x1d, 0xf0];
//!
//! let insn = Isa::ESP32.decode(&code, 0x4000_0000).unwrap();
//! assert_eq!(insn.opcode(), Opcode::Entry);
//! assert_eq!(insn.len(), 3);
//! assert_eq!(insn.operands(), &[Operand::Ar(1), Operand::Imm(32)]);
//! assert_eq!(insn.to_string(), "entry a1, 32");
//!
//! let listing: Vec<String> = Isa::ESP32
//!     .disassemble(&code, 0x4000_0000)
//!     .map(|(_, insn)| insn.unwrap().to_string())
//!     .collect();
//! assert_eq!(listing, ["entry a1, 32", "movi.n a8, 0", "retw.n"]);
//!
//! // The ESP8266 has no register windows.
//! assert!(Isa::ESP8266.decode(&code, 0).is_err());
//!
//! // Control flow information for building CFGs.
//! let j = Isa::ESP32.decode(&[0x06, 0x01, 0x00], 0x1000).unwrap();
//! assert_eq!(j.control_flow(), ControlFlow::Jump { target: 0x1008 });
//! ```

#![no_std]

mod decode;
mod instruction;
mod isa;
mod opcode;
mod pie;

pub use decode::{DecodeError, Disassembly};
pub use instruction::{ControlFlow, Instruction, MAX_OPERANDS, Mnemonic, Operand};
pub use isa::{
    Access, ESP32_SPECIAL_REGISTERS, ESP32_USER_REGISTERS, ESP32S2_SPECIAL_REGISTERS,
    ESP32S2_USER_REGISTERS, ESP32S3_SPECIAL_REGISTERS, ESP32S3_USER_REGISTERS,
    ESP8266_SPECIAL_REGISTERS, Features, Isa, SpecialRegister, UserRegister,
};
pub use opcode::Opcode;

#[doc = include_str!("../README.md")]
#[cfg(doctest)]
struct ReadmeDoctests;
