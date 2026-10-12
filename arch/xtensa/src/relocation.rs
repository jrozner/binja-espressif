//! ELF relocations, for relocatable objects (`.o` files and the members of
//! static libraries such as ESP-IDF's binary Wi-Fi, PHY and Bluetooth
//! blobs). Linked ESP-IDF ELFs carry none.
//!
//! The relocation numbers are binutils' (`include/elf/xtensa.h`), and the
//! semantics follow `elf_xtensa_do_reloc` in `bfd/elf32-xtensa.c`:
//!
//! * `R_XTENSA_32` (1): a 32-bit word, `S + A` added to what the word
//!   already holds. GNU as leaves part of the addend in the word
//!   ("partial in-place"), so the word is added to, not overwritten.
//! * `R_XTENSA_32_PCREL` (14): a 32-bit word, `S + A - P`.
//! * `R_XTENSA_SLOT0_OP` (20), and the old `R_XTENSA_OP0`..`OP2` (8..10):
//!   the PC-relative operand of the instruction at `P` (a branch, `j`, call,
//!   loop or `l32r`) is set to reach `S + A`, or the immediate of a `movi`
//!   or `movi.n` is set to `S + A`; see [`encode::set_field`]. A target the
//!   instruction cannot encode (out of range, misaligned, or an `l32r`
//!   literal at or after the instruction) is logged and the instruction
//!   left as it is. (`const16`, which binutils also relocates this way, is
//!   not an option of Espressif's cores, and is not decoded.)
//! * `R_XTENSA_ASM_EXPAND` (11), `R_XTENSA_ASM_SIMPLIFY` (12): relaxation
//!   hints for the linker; the code is correct as it is.
//! * `R_XTENSA_DIFF8/16/32` (17..19), `PDIFF` (57..59), `NDIFF` (60..62):
//!   label differences in debug and unwind sections. The assembler has
//!   already stored the difference; the linker only adjusts it when
//!   relaxation moves code, which loading an object does not.
//! * `R_XTENSA_SLOT1_OP`..`SLOT14_OP` and `SLOT1_ALT`..`SLOT14_ALT`: FLIX
//!   slots, which Espressif's cores do not have.
//!
//! Everything else (the dynamic and TLS relocations, and `SLOT0_ALT` for
//! `const16` and absolute `l32r`, which Espressif's cores do not have) is
//! left unhandled.

use binaryninja::architecture::CoreArchitecture;
use binaryninja::binary_view::BinaryView;
use binaryninja::low_level_il::LowLevelILRegularFunction;
use binaryninja::relocation::{
    CoreRelocationHandler, CustomRelocationHandlerHandle, Relocation, RelocationHandler,
    RelocationInfo, RelocationOperand, RelocationType,
};
use xtensa_isa::Isa;

use crate::architecture::guard;
use crate::encode;

pub const R_XTENSA_NONE: u64 = 0;
pub const R_XTENSA_32: u64 = 1;
pub const R_XTENSA_RTLD: u64 = 2;
pub const R_XTENSA_GLOB_DAT: u64 = 3;
pub const R_XTENSA_JMP_SLOT: u64 = 4;
pub const R_XTENSA_RELATIVE: u64 = 5;
pub const R_XTENSA_PLT: u64 = 6;
pub const R_XTENSA_OP0: u64 = 8;
pub const R_XTENSA_OP1: u64 = 9;
pub const R_XTENSA_OP2: u64 = 10;
pub const R_XTENSA_ASM_EXPAND: u64 = 11;
pub const R_XTENSA_ASM_SIMPLIFY: u64 = 12;
pub const R_XTENSA_32_PCREL: u64 = 14;
pub const R_XTENSA_GNU_VTINHERIT: u64 = 15;
pub const R_XTENSA_GNU_VTENTRY: u64 = 16;
pub const R_XTENSA_DIFF8: u64 = 17;
#[allow(dead_code)] // Handled through the R_XTENSA_DIFF8..=R_XTENSA_DIFF32 range.
pub const R_XTENSA_DIFF16: u64 = 18;
pub const R_XTENSA_DIFF32: u64 = 19;
pub const R_XTENSA_SLOT0_OP: u64 = 20;
pub const R_XTENSA_SLOT14_OP: u64 = 34;
pub const R_XTENSA_SLOT0_ALT: u64 = 35;
pub const R_XTENSA_SLOT14_ALT: u64 = 49;
pub const R_XTENSA_TLSDESC_FN: u64 = 50;
pub const R_XTENSA_TLS_CALL: u64 = 56;
pub const R_XTENSA_PDIFF8: u64 = 57;
pub const R_XTENSA_NDIFF32: u64 = 62;

/// What a relocation type does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Nothing to do: a hint, or a value the assembler already stored.
    Ignored,
    /// `R_XTENSA_32`: add `S + A` to the 32-bit word.
    Word,
    /// `R_XTENSA_32_PCREL`: store `S + A - P` in the 32-bit word.
    PcRelWord,
    /// `R_XTENSA_SLOT0_OP`: set the instruction's operand to `S + A`.
    Operand,
    /// Not supported.
    Unhandled,
}

pub fn kind(native_type: u64) -> Kind {
    match native_type {
        R_XTENSA_32 => Kind::Word,
        R_XTENSA_32_PCREL => Kind::PcRelWord,
        R_XTENSA_SLOT0_OP | R_XTENSA_OP0..=R_XTENSA_OP2 => Kind::Operand,
        R_XTENSA_NONE
        | R_XTENSA_ASM_EXPAND
        | R_XTENSA_ASM_SIMPLIFY
        | R_XTENSA_GNU_VTINHERIT
        | R_XTENSA_GNU_VTENTRY
        | R_XTENSA_DIFF8..=R_XTENSA_DIFF32
        | R_XTENSA_PDIFF8..=R_XTENSA_NDIFF32 => Kind::Ignored,
        // FLIX slots 1..14, operand and alternate.
        t if (R_XTENSA_SLOT0_OP + 1..=R_XTENSA_SLOT14_OP).contains(&t) => Kind::Ignored,
        t if (R_XTENSA_SLOT0_ALT + 1..=R_XTENSA_SLOT14_ALT).contains(&t) => Kind::Ignored,
        _ => Kind::Unhandled,
    }
}

/// The name of a relocation type, for messages.
pub fn name(native_type: u64) -> String {
    let name = match native_type {
        R_XTENSA_NONE => "R_XTENSA_NONE",
        R_XTENSA_32 => "R_XTENSA_32",
        R_XTENSA_RTLD => "R_XTENSA_RTLD",
        R_XTENSA_GLOB_DAT => "R_XTENSA_GLOB_DAT",
        R_XTENSA_JMP_SLOT => "R_XTENSA_JMP_SLOT",
        R_XTENSA_RELATIVE => "R_XTENSA_RELATIVE",
        R_XTENSA_PLT => "R_XTENSA_PLT",
        R_XTENSA_OP0 => "R_XTENSA_OP0",
        R_XTENSA_OP1 => "R_XTENSA_OP1",
        R_XTENSA_OP2 => "R_XTENSA_OP2",
        R_XTENSA_ASM_EXPAND => "R_XTENSA_ASM_EXPAND",
        R_XTENSA_ASM_SIMPLIFY => "R_XTENSA_ASM_SIMPLIFY",
        R_XTENSA_32_PCREL => "R_XTENSA_32_PCREL",
        R_XTENSA_SLOT0_OP => "R_XTENSA_SLOT0_OP",
        R_XTENSA_SLOT0_ALT => "R_XTENSA_SLOT0_ALT",
        t @ R_XTENSA_TLSDESC_FN..=R_XTENSA_TLS_CALL => {
            return format!("TLS relocation {t}");
        }
        t => return format!("relocation type {t}"),
    };
    name.to_string()
}

/// Why a relocation was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not a relocation [`apply`] handles.
    NotApplicable,
    /// The relocated bytes run past the end of the data.
    Truncated,
    /// The instruction cannot be relocated to the target.
    Encode(encode::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotApplicable => f.write_str("not applicable"),
            Error::Truncated => f.write_str("past the end of the data"),
            Error::Encode(e) => e.fmt(f),
        }
    }
}

/// Applies the relocation `native_type` at `pc`, whose bytes start `data`,
/// against the symbol value `symbol` and the addend `addend`. On error,
/// `data` is unchanged.
pub fn apply(
    isa: &Isa,
    native_type: u64,
    data: &mut [u8],
    pc: u32,
    symbol: u32,
    addend: u32,
) -> Result<(), Error> {
    let value = symbol.wrapping_add(addend);
    match kind(native_type) {
        Kind::Word => {
            let word = data.first_chunk_mut::<4>().ok_or(Error::Truncated)?;
            *word = u32::from_le_bytes(*word).wrapping_add(value).to_le_bytes();
        }
        Kind::PcRelWord => {
            let word = data.first_chunk_mut::<4>().ok_or(Error::Truncated)?;
            *word = value.wrapping_sub(pc).to_le_bytes();
        }
        Kind::Operand => {
            let bytes = encode::retarget(isa, data, pc, value).map_err(Error::Encode)?;
            if !encode::write(data, Some(bytes)) {
                return Err(Error::Truncated);
            }
        }
        Kind::Ignored | Kind::Unhandled => return Err(Error::NotApplicable),
    }
    Ok(())
}

/// The length of the instruction starting with `byte0`: 2 for the narrow
/// (op0 8..=0xd) and 3 for the others. The ESP32-S3's 4-byte PIE
/// instructions have no relocatable operand.
pub fn instruction_len(byte0: u8) -> usize {
    match byte0 & 0xf {
        0x8..=0xd => 2,
        _ => 3,
    }
}

pub struct XtensaElfRelocationHandler {
    handle: CoreRelocationHandler,
    custom_handle: CustomRelocationHandlerHandle<Self>,
    isa: Isa,
}

impl XtensaElfRelocationHandler {
    pub fn new(
        isa: Isa,
        custom_handle: CustomRelocationHandlerHandle<Self>,
        handle: CoreRelocationHandler,
    ) -> Self {
        XtensaElfRelocationHandler {
            handle,
            custom_handle,
            isa,
        }
    }
}

impl AsRef<CoreRelocationHandler> for XtensaElfRelocationHandler {
    fn as_ref(&self) -> &CoreRelocationHandler {
        &self.handle
    }
}

impl RelocationHandler for XtensaElfRelocationHandler {
    type Handle = CustomRelocationHandlerHandle<Self>;

    fn get_relocation_info(
        &self,
        bv: &BinaryView,
        _arch: &CoreArchitecture,
        info: &mut [RelocationInfo],
    ) -> bool {
        guard("get_relocation_info", 0, || {
            for reloc in info.iter_mut() {
                describe(bv, reloc);
            }
            Some(true)
        })
        .unwrap_or(false)
    }

    fn apply_relocation(
        &self,
        _bv: &BinaryView,
        _arch: &CoreArchitecture,
        reloc: &Relocation,
        dest: &mut [u8],
    ) -> bool {
        let info = reloc.info();
        guard("apply_relocation", info.address, || {
            let pc = info.address as u32;
            let symbol = reloc.target() as u32;
            let result = apply(
                &self.isa,
                info.native_type,
                dest,
                pc,
                symbol,
                info.addend as u32,
            );
            if let Err(e) = result {
                tracing::warn!(
                    "{} at {pc:#x} to {:#x} not applied: {e}",
                    name(info.native_type),
                    symbol.wrapping_add(info.addend as u32),
                );
            }
            Some(result.is_ok())
        })
        .unwrap_or(false)
    }

    fn get_operand_for_external_relocation(
        &self,
        _data: &[u8],
        _addr: u64,
        _il: &LowLevelILRegularFunction,
        reloc: &Relocation,
    ) -> RelocationOperand {
        match kind(reloc.info().native_type) {
            // A call or jump to an external symbol: its target is the
            // extern's address, which Binary Ninja finds in the IL.
            Kind::Operand => RelocationOperand::AutocoerceExternPtr,
            _ => RelocationOperand::Invalid,
        }
    }

    fn handle(&self) -> Self::Handle {
        self.custom_handle
    }
}

/// Fills in what Binary Ninja needs to know of one relocation.
fn describe(bv: &BinaryView, reloc: &mut RelocationInfo) {
    reloc.type_ = RelocationType::StandardRelocationType;
    reloc.base_relative = false;
    reloc.has_sign = false;
    reloc.implicit_addend = false;
    match kind(reloc.native_type) {
        Kind::Word => {
            reloc.pc_relative = false;
            reloc.size = 4;
            reloc.truncate_size = 4;
            // Literal pool entries and data pointers, never code.
            reloc.data_relocation = true;
        }
        Kind::PcRelWord => {
            reloc.pc_relative = true;
            reloc.size = 4;
            reloc.truncate_size = 4;
        }
        Kind::Operand => {
            reloc.pc_relative = true;
            let byte0 = bv.read_vec(reloc.address, 1);
            reloc.size = byte0.first().map_or(3, |&b| instruction_len(b));
            reloc.truncate_size = reloc.size;
        }
        Kind::Ignored => reloc.type_ = RelocationType::IgnoredRelocation,
        Kind::Unhandled => {
            reloc.type_ = RelocationType::UnhandledRelocation;
            tracing::warn!(
                "unhandled {} at {:#x}",
                name(reloc.native_type),
                reloc.address
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISAS: [Isa; 4] = [Isa::ESP8266, Isa::ESP32, Isa::ESP32S2, Isa::ESP32S3];

    #[test]
    fn kinds() {
        assert_eq!(kind(1), Kind::Word);
        assert_eq!(kind(14), Kind::PcRelWord);
        assert_eq!(kind(20), Kind::Operand);
        for t in [8, 9, 10] {
            assert_eq!(kind(t), Kind::Operand);
        }
        for t in [0, 11, 12, 15, 16, 17, 18, 19, 57, 58, 59, 60, 61, 62] {
            assert_eq!(kind(t), Kind::Ignored, "{t}");
        }
        for t in 21..=34 {
            assert_eq!(kind(t), Kind::Ignored, "SLOT{}_OP", t - 20);
        }
        for t in 36..=49 {
            assert_eq!(kind(t), Kind::Ignored, "SLOT{}_ALT", t - 35);
        }
        // Dynamic, SLOT0_ALT, TLS and unknown types.
        for t in [
            2, 3, 4, 5, 6, 7, 13, 35, 50, 51, 52, 53, 54, 55, 56, 63, 255,
        ] {
            assert_eq!(kind(t), Kind::Unhandled, "{t}");
        }
    }

    #[test]
    fn words() {
        for isa in ISAS {
            // R_XTENSA_32 adds to the in-place part of the addend.
            let mut data = [0x2c, 0, 0, 0, 0xaa];
            assert_eq!(
                apply(&isa, R_XTENSA_32, &mut data, 0x1_0000, 0x1_0240, 4),
                Ok(())
            );
            assert_eq!(data, [0x70, 0x02, 0x01, 0x00, 0xaa]);
            // A negative addend wraps.
            let mut data = [0; 4];
            assert_eq!(
                apply(&isa, R_XTENSA_32, &mut data, 0, 0x100, -4i32 as u32),
                Ok(())
            );
            assert_eq!(u32::from_le_bytes(data), 0xfc);
            // R_XTENSA_32_PCREL overwrites.
            let mut data = [0xff; 4];
            assert_eq!(
                apply(&isa, R_XTENSA_32_PCREL, &mut data, 0x1_0010, 0x1_0000, 8),
                Ok(())
            );
            assert_eq!(u32::from_le_bytes(data), -8i32 as u32);
            let mut short = [0u8; 3];
            assert_eq!(
                apply(&isa, R_XTENSA_32, &mut short, 0, 0, 0),
                Err(Error::Truncated)
            );
            assert_eq!(short, [0; 3]);
        }
    }

    #[test]
    fn operands() {
        for isa in ISAS {
            // call0 at 0x1_0137 to add3 at 0x1_0020 (as in an object).
            let mut data = [0x05, 0x00, 0x00, 0x99];
            assert_eq!(
                apply(&isa, R_XTENSA_SLOT0_OP, &mut data, 0x1_0137, 0x1_0020, 0),
                Ok(())
            );
            let insn = isa.decode(&data, 0x1_0137).unwrap();
            assert_eq!(
                insn.control_flow(),
                xtensa_isa::ControlFlow::Call {
                    target: 0x1_0020,
                    window: 0
                }
            );
            assert_eq!(data[3], 0x99);
            // l32r a2, .literal + 0xc
            let mut data = [0x21, 0x00, 0x00];
            assert_eq!(
                apply(&isa, R_XTENSA_SLOT0_OP, &mut data, 0x1_0170, 0x1_0000, 0xc),
                Ok(())
            );
            assert_eq!(
                isa.decode(&data, 0x1_0170).unwrap().operands(),
                [
                    xtensa_isa::Operand::Ar(2),
                    xtensa_isa::Operand::Literal(0x1_000c)
                ]
            );
            // A literal after its l32r is left alone.
            let before = data;
            assert_eq!(
                apply(&isa, R_XTENSA_SLOT0_OP, &mut data, 0x1_0170, 0x1_0200, 0),
                Err(Error::Encode(encode::Error::LiteralAfterUse))
            );
            assert_eq!(data, before);
            // So is a misaligned call target, and an out-of-range branch.
            let mut data = [0x05, 0x00, 0x00];
            assert_eq!(
                apply(&isa, R_XTENSA_SLOT0_OP, &mut data, 0, 0x102, 0),
                Err(Error::Encode(encode::Error::Misaligned))
            );
            assert_eq!(data, [0x05, 0x00, 0x00]);
            let mut data = [0x8c, 0xd4];
            assert_eq!(
                apply(&isa, R_XTENSA_SLOT0_OP, &mut data, 0x67, 0x100, 0),
                Err(Error::Encode(encode::Error::OutOfRange))
            );
            assert_eq!(data, [0x8c, 0xd4]);
            // Ignored types do nothing.
            let mut data = [1, 2, 3, 4];
            assert_eq!(
                apply(&isa, R_XTENSA_DIFF32, &mut data, 0, 0x100, 0),
                Err(Error::NotApplicable)
            );
            assert_eq!(data, [1, 2, 3, 4]);
        }
    }

    #[test]
    fn instruction_lengths() {
        assert_eq!(instruction_len(0x8c), 2); // beqz.n
        assert_eq!(instruction_len(0x2d), 2); // mov.n
        assert_eq!(instruction_len(0x25), 3); // call8
        assert_eq!(instruction_len(0x21), 3); // l32r
        assert_eq!(instruction_len(0x76), 3); // loop
    }
}
