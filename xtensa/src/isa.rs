//! Processor configurations.
//!
//! Xtensa is a configurable architecture: every core implements a common base
//! ISA plus a vendor-selected set of options. An [`Isa`] captures the options
//! and special/user registers a particular core implements, so the decoder can
//! reject encodings that would raise an illegal-instruction exception on it.

use core::ops::{BitOr, BitOrAssign};

/// A set of optional ISA features.
///
/// Features that both supported Espressif cores implement (code density,
/// 16-bit multiply, `mull`, `nsa`, debug, exceptions, interrupts, TLB access)
/// are still listed so custom configurations can describe smaller cores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Features(u32);

impl Features {
    /// No optional features.
    pub const NONE: Self = Self(0);
    /// Code Density option: 16-bit "narrow" instructions (`add.n`, `l32i.n`, ...).
    pub const DENSITY: Self = Self(1 << 0);
    /// Windowed Register option (`entry`, `retw`, `call4`, `movsp`, ...).
    pub const WINDOWED: Self = Self(1 << 1);
    /// Boolean option (`bt`, `bf`, `andb`, `any4`, `movt`, ...).
    pub const BOOLEAN: Self = Self(1 << 2);
    /// Loop option (`loop`, `loopnez`, `loopgtz`).
    pub const LOOP: Self = Self(1 << 3);
    /// MAC16 option (`mul.aa.ll`, `mula.dd.hh.ldinc`, `ldinc`, ...).
    pub const MAC16: Self = Self(1 << 4);
    /// 16-bit integer multiply option (`mul16u`, `mul16s`).
    pub const MUL16: Self = Self(1 << 5);
    /// 32-bit integer multiply option (`mull`).
    pub const MUL32: Self = Self(1 << 6);
    /// High half of 32-bit multiply (`muluh`, `mulsh`).
    pub const MUL32_HIGH: Self = Self(1 << 7);
    /// 32-bit integer divide option (`quou`, `quos`, `remu`, `rems`).
    pub const DIV32: Self = Self(1 << 8);
    /// Normalization shift amount (`nsa`, `nsau`).
    pub const NSA: Self = Self(1 << 9);
    /// Integer min/max (`min`, `max`, `minu`, `maxu`).
    pub const MINMAX: Self = Self(1 << 10);
    /// Sign extend (`sext`).
    pub const SEXT: Self = Self(1 << 11);
    /// Signed clamp (`clamps`).
    pub const CLAMPS: Self = Self(1 << 12);
    /// Single-precision floating point coprocessor, including the divide and
    /// square-root assist instructions present on the ESP32. The compares,
    /// `movf.s` and `movt.s` also need [`Features::BOOLEAN`], and the
    /// ESP32-S3 PIE instructions that use `f` registers need this too.
    pub const FP: Self = Self(1 << 13);
    /// Conditional store (`s32c1i`).
    pub const S32C1I: Self = Self(1 << 14);
    /// Multiprocessor synchronization (`l32ai`, `s32ri`).
    pub const MP_SYNC: Self = Self(1 << 15);
    /// Debug option (`break`, `break.n`, `rfdo`, `rfdd`).
    pub const DEBUG: Self = Self(1 << 16);
    /// On-chip-debug data register transfers (`lddr32.p`, `sddr32.p`).
    pub const DEBUG_DDR: Self = Self(1 << 17);
    /// Exception option (`rfe`, `rfde`, `syscall`, `simcall`, `excw`).
    pub const EXCEPTIONS: Self = Self(1 << 18);
    /// Interrupt option (`rfi`, `rsil`, `waiti`).
    pub const INTERRUPTS: Self = Self(1 << 19);
    /// TLB / region protection access (`witlb`, `pdtlb`, ...).
    pub const TLB: Self = Self(1 << 20);
    /// External register access (`rer`, `wer`).
    pub const EXTERNAL_REGS: Self = Self(1 << 21);
    /// Non-buffered store (`s32nb`).
    pub const S32NB: Self = Self(1 << 22);
    /// ESP32 double-precision floating point assist (`f64addc`, `rf64r`, ...).
    pub const ESP32_DFP_ACCEL: Self = Self(1 << 23);
    /// ESP32 `EXPSTATE` TIE extension (`setb_expstate`, `read_impwire`, ...).
    pub const ESP32_EXPSTATE: Self = Self(1 << 24);
    /// Set if less than (`salt`, `saltu`), new in Xtensa LX7.
    pub const SALT: Self = Self(1 << 25);
    /// ESP32-S2 dedicated GPIO TIE extension (`set_bit_gpio_out`,
    /// `get_gpio_in`, ...).
    pub const ESP32S2_GPIO: Self = Self(1 << 26);
    /// ESP32-S3 Processor Instruction Extensions: the `ee.*` vector
    /// instructions, `ld.qr`/`st.qr`/`mv.qr` and the 4-byte instruction format.
    pub const ESP32S3_PIE: Self = Self(1 << 27);

    /// Returns true if every feature in `other` is also in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union of two feature sets, usable in `const` contexts.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The feature set with everything in `other` removed.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl BitOr for Features {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for Features {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

/// How an instruction accesses a special or user register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    /// `rsr` / `rur`.
    Read,
    /// `wsr` / `wur`.
    Write,
    /// `xsr`.
    Exchange,
}

/// A special register (`rsr`/`wsr`/`xsr` operand) implemented by a core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpecialRegister {
    /// Register number as encoded in the instruction.
    pub number: u8,
    /// Lower-case name, e.g. `"sar"`.
    pub name: &'static str,
    /// Whether `rsr` may access it.
    pub read: bool,
    /// Whether `wsr` may access it.
    pub write: bool,
    /// Whether `xsr` may access it.
    pub exchange: bool,
}

impl SpecialRegister {
    /// Returns true if this register may be accessed in the given way.
    #[must_use]
    pub const fn allows(&self, access: Access) -> bool {
        match access {
            Access::Read => self.read,
            Access::Write => self.write,
            Access::Exchange => self.exchange,
        }
    }
}

/// A user register (`rur`/`wur` operand) implemented by a core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserRegister {
    /// Register number as encoded in the instruction.
    pub number: u8,
    /// Lower-case name, e.g. `"threadptr"`.
    pub name: &'static str,
}

/// A processor configuration to decode for.
///
/// The presets ([`Isa::ESP32`], ...) describe real cores. A custom
/// configuration must keep its register tables consistent with its features:
/// the decoder takes special and user registers from the tables alone, so,
/// for example, a configuration without [`Features::BOOLEAN`] should not list
/// the `br` special register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Isa {
    /// Optional instruction groups implemented by the core.
    pub features: Features,
    /// Special registers implemented by the core.
    pub special_registers: &'static [SpecialRegister],
    /// User registers implemented by the core.
    pub user_registers: &'static [UserRegister],
}

impl Isa {
    /// The Tensilica LX106 core used by the ESP8266.
    pub const ESP8266: Isa = Isa {
        features: Features::DENSITY
            .union(Features::MUL16)
            .union(Features::MUL32)
            .union(Features::NSA)
            .union(Features::DEBUG)
            .union(Features::EXCEPTIONS)
            .union(Features::INTERRUPTS)
            .union(Features::TLB)
            .union(Features::EXTERNAL_REGS),
        special_registers: ESP8266_SPECIAL_REGISTERS,
        user_registers: &[],
    };

    /// The Tensilica LX6 cores used by the ESP32.
    pub const ESP32: Isa = Isa {
        features: Isa::ESP8266
            .features
            .union(Features::WINDOWED)
            .union(Features::BOOLEAN)
            .union(Features::LOOP)
            .union(Features::MAC16)
            .union(Features::MUL32_HIGH)
            .union(Features::DIV32)
            .union(Features::MINMAX)
            .union(Features::SEXT)
            .union(Features::CLAMPS)
            .union(Features::FP)
            .union(Features::S32C1I)
            .union(Features::MP_SYNC)
            .union(Features::DEBUG_DDR)
            .union(Features::S32NB)
            .union(Features::ESP32_DFP_ACCEL)
            .union(Features::ESP32_EXPSTATE),
        special_registers: ESP32_SPECIAL_REGISTERS,
        user_registers: ESP32_USER_REGISTERS,
    };

    /// The Tensilica LX7 core used by the ESP32-S2.
    ///
    /// Compared with the ESP32 it lacks the FPU, MAC16, loop, boolean and
    /// conditional store options and the ESP32 TIE extensions, and adds
    /// `salt`/`saltu` and dedicated GPIO instructions.
    pub const ESP32S2: Isa = Isa {
        features: Isa::ESP32
            .features
            .difference(Features::BOOLEAN)
            .difference(Features::LOOP)
            .difference(Features::MAC16)
            .difference(Features::FP)
            .difference(Features::S32C1I)
            .difference(Features::ESP32_DFP_ACCEL)
            .difference(Features::ESP32_EXPSTATE)
            .union(Features::SALT)
            .union(Features::ESP32S2_GPIO),
        special_registers: ESP32S2_SPECIAL_REGISTERS,
        user_registers: ESP32S2_USER_REGISTERS,
    };

    /// The Tensilica LX7 cores used by the ESP32-S3.
    ///
    /// It has the ESP32's options except the ESP32 TIE extensions, and adds
    /// `salt`/`saltu` and the Processor Instruction Extensions (PIE), whose
    /// instructions may be 4 bytes long.
    pub const ESP32S3: Isa = Isa {
        features: Isa::ESP32
            .features
            .difference(Features::ESP32_DFP_ACCEL)
            .difference(Features::ESP32_EXPSTATE)
            .union(Features::SALT)
            .union(Features::ESP32S3_PIE),
        special_registers: ESP32S3_SPECIAL_REGISTERS,
        user_registers: ESP32S3_USER_REGISTERS,
    };

    /// Returns true if the core implements all of `features`.
    #[must_use]
    pub const fn has(&self, features: Features) -> bool {
        self.features.contains(features)
    }

    /// Looks up a special register by number, if the core allows `access` to it.
    #[must_use]
    pub fn special_register(&self, number: u8, access: Access) -> Option<&'static SpecialRegister> {
        self.special_registers
            .iter()
            .find(|sr| sr.number == number && sr.allows(access))
    }

    /// Looks up a user register by number.
    #[must_use]
    pub fn user_register(&self, number: u8) -> Option<&'static UserRegister> {
        self.user_registers.iter().find(|ur| ur.number == number)
    }
}

const fn rwx(number: u8, name: &'static str) -> SpecialRegister {
    SpecialRegister {
        number,
        name,
        read: true,
        write: true,
        exchange: true,
    }
}

const fn rw(number: u8, name: &'static str) -> SpecialRegister {
    SpecialRegister {
        number,
        name,
        read: true,
        write: true,
        exchange: false,
    }
}

const fn ro(number: u8, name: &'static str) -> SpecialRegister {
    SpecialRegister {
        number,
        name,
        read: true,
        write: false,
        exchange: false,
    }
}

const fn wo(number: u8, name: &'static str) -> SpecialRegister {
    SpecialRegister {
        number,
        name,
        read: false,
        write: true,
        exchange: false,
    }
}

/// Special registers of the ESP8266 (LX106).
pub static ESP8266_SPECIAL_REGISTERS: &[SpecialRegister] = &[
    rwx(3, "sar"),
    rwx(5, "litbase"),
    wo(89, "mmid"),
    rwx(96, "ibreakenable"),
    rwx(104, "ddr"),
    rwx(128, "ibreaka0"),
    rwx(144, "dbreaka0"),
    rwx(160, "dbreakc0"),
    rw(176, "configid0"),
    rwx(177, "epc1"),
    rwx(178, "epc2"),
    rwx(179, "epc3"),
    rwx(192, "depc"),
    rwx(194, "eps2"),
    rwx(195, "eps3"),
    ro(208, "configid1"),
    rwx(209, "excsave1"),
    rwx(210, "excsave2"),
    rwx(211, "excsave3"),
    ro(226, "interrupt"),
    wo(226, "intset"),
    wo(227, "intclear"),
    rwx(228, "intenable"),
    rwx(230, "ps"),
    rwx(231, "vecbase"),
    rwx(232, "exccause"),
    rwx(233, "debugcause"),
    rwx(234, "ccount"),
    ro(235, "prid"),
    rwx(236, "icount"),
    rwx(237, "icountlevel"),
    rwx(238, "excvaddr"),
    rwx(240, "ccompare0"),
];

/// Special registers of the ESP32 (LX6).
pub static ESP32_SPECIAL_REGISTERS: &[SpecialRegister] = &[
    rwx(0, "lbeg"),
    rwx(1, "lend"),
    rwx(2, "lcount"),
    rwx(3, "sar"),
    rwx(4, "br"),
    rwx(5, "litbase"),
    rwx(12, "scompare1"),
    rwx(16, "acclo"),
    rwx(17, "acchi"),
    rwx(32, "m0"),
    rwx(33, "m1"),
    rwx(34, "m2"),
    rwx(35, "m3"),
    rwx(72, "windowbase"),
    rwx(73, "windowstart"),
    wo(89, "mmid"),
    rwx(96, "ibreakenable"),
    rwx(97, "memctl"),
    rwx(99, "atomctl"),
    rwx(104, "ddr"),
    rwx(128, "ibreaka0"),
    rwx(129, "ibreaka1"),
    rwx(144, "dbreaka0"),
    rwx(145, "dbreaka1"),
    rwx(160, "dbreakc0"),
    rwx(161, "dbreakc1"),
    rw(176, "configid0"),
    rwx(177, "epc1"),
    rwx(178, "epc2"),
    rwx(179, "epc3"),
    rwx(180, "epc4"),
    rwx(181, "epc5"),
    rwx(182, "epc6"),
    rwx(183, "epc7"),
    rwx(192, "depc"),
    rwx(194, "eps2"),
    rwx(195, "eps3"),
    rwx(196, "eps4"),
    rwx(197, "eps5"),
    rwx(198, "eps6"),
    rwx(199, "eps7"),
    ro(208, "configid1"),
    rwx(209, "excsave1"),
    rwx(210, "excsave2"),
    rwx(211, "excsave3"),
    rwx(212, "excsave4"),
    rwx(213, "excsave5"),
    rwx(214, "excsave6"),
    rwx(215, "excsave7"),
    rwx(224, "cpenable"),
    ro(226, "interrupt"),
    wo(226, "intset"),
    wo(227, "intclear"),
    rwx(228, "intenable"),
    rwx(230, "ps"),
    rwx(231, "vecbase"),
    rwx(232, "exccause"),
    rwx(233, "debugcause"),
    rwx(234, "ccount"),
    ro(235, "prid"),
    rwx(236, "icount"),
    rwx(237, "icountlevel"),
    rwx(238, "excvaddr"),
    rwx(240, "ccompare0"),
    rwx(241, "ccompare1"),
    rwx(242, "ccompare2"),
    rwx(244, "misc0"),
    rwx(245, "misc1"),
    rwx(246, "misc2"),
    rwx(247, "misc3"),
];

/// Special registers of the ESP32-S2 (LX7).
pub static ESP32S2_SPECIAL_REGISTERS: &[SpecialRegister] = &[
    rwx(3, "sar"),
    rwx(5, "litbase"),
    rwx(72, "windowbase"),
    rwx(73, "windowstart"),
    wo(89, "mmid"),
    rwx(95, "eraccess"),
    rwx(96, "ibreakenable"),
    rwx(97, "memctl"),
    rwx(104, "ddr"),
    rwx(128, "ibreaka0"),
    rwx(129, "ibreaka1"),
    rwx(144, "dbreaka0"),
    rwx(145, "dbreaka1"),
    rwx(160, "dbreakc0"),
    rwx(161, "dbreakc1"),
    rw(176, "configid0"),
    rwx(177, "epc1"),
    rwx(178, "epc2"),
    rwx(179, "epc3"),
    rwx(180, "epc4"),
    rwx(181, "epc5"),
    rwx(182, "epc6"),
    rwx(183, "epc7"),
    rwx(192, "depc"),
    rwx(194, "eps2"),
    rwx(195, "eps3"),
    rwx(196, "eps4"),
    rwx(197, "eps5"),
    rwx(198, "eps6"),
    rwx(199, "eps7"),
    ro(208, "configid1"),
    rwx(209, "excsave1"),
    rwx(210, "excsave2"),
    rwx(211, "excsave3"),
    rwx(212, "excsave4"),
    rwx(213, "excsave5"),
    rwx(214, "excsave6"),
    rwx(215, "excsave7"),
    rwx(224, "cpenable"),
    ro(226, "interrupt"),
    wo(226, "intset"),
    wo(227, "intclear"),
    rwx(228, "intenable"),
    rwx(230, "ps"),
    rwx(231, "vecbase"),
    rwx(232, "exccause"),
    rwx(233, "debugcause"),
    rwx(234, "ccount"),
    ro(235, "prid"),
    rwx(236, "icount"),
    rwx(237, "icountlevel"),
    rwx(238, "excvaddr"),
    rwx(240, "ccompare0"),
    rwx(241, "ccompare1"),
    rwx(242, "ccompare2"),
    rwx(244, "misc0"),
    rwx(245, "misc1"),
    rwx(246, "misc2"),
    rwx(247, "misc3"),
];

/// User registers of the ESP32-S2 (LX7).
pub static ESP32S2_USER_REGISTERS: &[UserRegister] = &[
    UserRegister {
        number: 0,
        name: "gpio_out",
    },
    UserRegister {
        number: 231,
        name: "threadptr",
    },
];

/// Special registers of the ESP32-S3 (LX7).
pub static ESP32S3_SPECIAL_REGISTERS: &[SpecialRegister] = &[
    rwx(0, "lbeg"),
    rwx(1, "lend"),
    rwx(2, "lcount"),
    rwx(3, "sar"),
    rwx(4, "br"),
    rwx(5, "litbase"),
    rwx(12, "scompare1"),
    rwx(16, "acclo"),
    rwx(17, "acchi"),
    rwx(32, "m0"),
    rwx(33, "m1"),
    rwx(34, "m2"),
    rwx(35, "m3"),
    rwx(72, "windowbase"),
    rwx(73, "windowstart"),
    wo(89, "mmid"),
    rwx(95, "eraccess"),
    rwx(96, "ibreakenable"),
    rwx(97, "memctl"),
    rwx(99, "atomctl"),
    rwx(104, "ddr"),
    rwx(128, "ibreaka0"),
    rwx(129, "ibreaka1"),
    rwx(144, "dbreaka0"),
    rwx(145, "dbreaka1"),
    rwx(160, "dbreakc0"),
    rwx(161, "dbreakc1"),
    rw(176, "configid0"),
    rwx(177, "epc1"),
    rwx(178, "epc2"),
    rwx(179, "epc3"),
    rwx(180, "epc4"),
    rwx(181, "epc5"),
    rwx(182, "epc6"),
    rwx(183, "epc7"),
    rwx(192, "depc"),
    rwx(194, "eps2"),
    rwx(195, "eps3"),
    rwx(196, "eps4"),
    rwx(197, "eps5"),
    rwx(198, "eps6"),
    rwx(199, "eps7"),
    ro(208, "configid1"),
    rwx(209, "excsave1"),
    rwx(210, "excsave2"),
    rwx(211, "excsave3"),
    rwx(212, "excsave4"),
    rwx(213, "excsave5"),
    rwx(214, "excsave6"),
    rwx(215, "excsave7"),
    rwx(224, "cpenable"),
    ro(226, "interrupt"),
    wo(226, "intset"),
    wo(227, "intclear"),
    rwx(228, "intenable"),
    rwx(230, "ps"),
    rwx(231, "vecbase"),
    rwx(232, "exccause"),
    rwx(233, "debugcause"),
    rwx(234, "ccount"),
    ro(235, "prid"),
    rwx(236, "icount"),
    rwx(237, "icountlevel"),
    rwx(238, "excvaddr"),
    rwx(240, "ccompare0"),
    rwx(241, "ccompare1"),
    rwx(242, "ccompare2"),
    rwx(244, "misc0"),
    rwx(245, "misc1"),
    rwx(246, "misc2"),
    rwx(247, "misc3"),
];

/// User registers of the ESP32-S3 (LX7).
pub static ESP32S3_USER_REGISTERS: &[UserRegister] = &[
    UserRegister {
        number: 0,
        name: "accx_0",
    },
    UserRegister {
        number: 1,
        name: "accx_1",
    },
    UserRegister {
        number: 2,
        name: "qacc_h_0",
    },
    UserRegister {
        number: 3,
        name: "qacc_h_1",
    },
    UserRegister {
        number: 4,
        name: "qacc_h_2",
    },
    UserRegister {
        number: 5,
        name: "qacc_h_3",
    },
    UserRegister {
        number: 6,
        name: "qacc_h_4",
    },
    UserRegister {
        number: 7,
        name: "qacc_l_0",
    },
    UserRegister {
        number: 8,
        name: "qacc_l_1",
    },
    UserRegister {
        number: 9,
        name: "qacc_l_2",
    },
    UserRegister {
        number: 10,
        name: "qacc_l_3",
    },
    UserRegister {
        number: 11,
        name: "qacc_l_4",
    },
    UserRegister {
        number: 12,
        name: "gpio_out",
    },
    UserRegister {
        number: 13,
        name: "sar_byte",
    },
    UserRegister {
        number: 14,
        name: "fft_bit_width",
    },
    UserRegister {
        number: 15,
        name: "ua_state_0",
    },
    UserRegister {
        number: 16,
        name: "ua_state_1",
    },
    UserRegister {
        number: 17,
        name: "ua_state_2",
    },
    UserRegister {
        number: 18,
        name: "ua_state_3",
    },
    UserRegister {
        number: 231,
        name: "threadptr",
    },
    UserRegister {
        number: 232,
        name: "fcr",
    },
    UserRegister {
        number: 233,
        name: "fsr",
    },
];

/// User registers of the ESP32 (LX6).
pub static ESP32_USER_REGISTERS: &[UserRegister] = &[
    UserRegister {
        number: 230,
        name: "expstate",
    },
    UserRegister {
        number: 231,
        name: "threadptr",
    },
    UserRegister {
        number: 232,
        name: "fcr",
    },
    UserRegister {
        number: 233,
        name: "fsr",
    },
    UserRegister {
        number: 234,
        name: "f64r_lo",
    },
    UserRegister {
        number: 235,
        name: "f64r_hi",
    },
    UserRegister {
        number: 236,
        name: "f64s",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esp32_is_superset_of_esp8266() {
        assert!(Isa::ESP32.features.contains(Isa::ESP8266.features));
        assert!(!Isa::ESP8266.has(Features::WINDOWED));
    }

    #[test]
    fn register_access_rules() {
        let isa = Isa::ESP32;
        assert_eq!(
            isa.special_register(226, Access::Read).unwrap().name,
            "interrupt"
        );
        assert_eq!(
            isa.special_register(226, Access::Write).unwrap().name,
            "intset"
        );
        assert!(isa.special_register(226, Access::Exchange).is_none());
        assert!(isa.special_register(235, Access::Write).is_none());
        assert!(Isa::ESP8266.special_register(72, Access::Read).is_none());
        assert_eq!(isa.user_register(231).unwrap().name, "threadptr");
        assert!(Isa::ESP8266.user_register(231).is_none());
    }

    #[test]
    fn tables_are_sorted_and_unique() {
        for table in [
            ESP32_SPECIAL_REGISTERS,
            ESP32S2_SPECIAL_REGISTERS,
            ESP8266_SPECIAL_REGISTERS,
        ] {
            for pair in table.windows(2) {
                assert!(
                    pair[0].number < pair[1].number
                        || (pair[0].number == pair[1].number && pair[0].name != pair[1].name),
                    "{pair:?}"
                );
            }
        }
    }
}
