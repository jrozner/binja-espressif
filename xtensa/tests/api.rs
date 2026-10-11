//! Tests of the public API beyond instruction text, which the objdump
//! comparisons cover.

use xtensa::{ControlFlow, DecodeError, Features, Isa, Opcode, Operand};

fn decode(isa: Isa, bytes: &[u8], address: u32) -> xtensa::Instruction {
    isa.decode(bytes, address)
        .unwrap_or_else(|e| panic!("{bytes:02x?}: {e}"))
}

#[test]
fn lengths() {
    assert_eq!(Isa::ESP32.instruction_length(0x36), Some(3));
    assert_eq!(Isa::ESP32.instruction_length(0x1d), Some(2));
    assert_eq!(Isa::ESP32.instruction_length(0x0e), None);
    assert_eq!(Isa::ESP32.instruction_length(0xff), None);
    let no_density = Isa {
        features: Features::NONE,
        ..Isa::ESP8266
    };
    assert_eq!(no_density.instruction_length(0x1d), None);
    assert_eq!(
        no_density.decode(&[0x1d, 0xf0], 0),
        Err(DecodeError::Invalid)
    );
}

#[test]
fn errors() {
    assert_eq!(Isa::ESP32.decode(&[], 0), Err(DecodeError::Truncated));
    assert_eq!(
        Isa::ESP32.decode(&[0x36, 0x41], 0),
        Err(DecodeError::Truncated)
    );
    assert_eq!(Isa::ESP32.decode(&[0x1d], 0), Err(DecodeError::Truncated));
    assert_eq!(
        Isa::ESP32.decode(&[0x00, 0x00, 0x00], 0).unwrap().opcode(),
        Opcode::Ill
    );
    assert_eq!(
        Isa::ESP32.decode(&[0x00, 0x01, 0x00], 0),
        Err(DecodeError::Invalid)
    );
    assert_eq!(
        Isa::ESP32.decode(&[0x0f, 0, 0], 0),
        Err(DecodeError::Invalid)
    );
    assert_eq!(DecodeError::Invalid.to_string(), "invalid instruction");
}

#[test]
fn trailing_bytes_are_ignored() {
    let insn = decode(Isa::ESP32, &[0x1d, 0xf0, 0xff, 0xff], 0);
    assert_eq!(insn.opcode(), Opcode::RetwN);
    assert_eq!(insn.len(), 2);
    assert_eq!(insn.raw(), 0xf01d);
}

#[test]
fn esp8266_rejects_esp32_only_instructions() {
    let esp32_only: &[&[u8]] = &[
        &[0x36, 0x41, 0x00], // entry a1, 32
        &[0x1d, 0xf0],       // retw.n
        &[0x76, 0x80, 0x05], // loop a0, ...
        &[0x00, 0x34, 0x0a], // add.s (FP)
        &[0x04, 0x00, 0x74], // mul.aa.ll (MAC16)
        &[0x70, 0x02, 0x03], // rur.threadptr a0
        &[0x00, 0x00, 0x03], // rsr.lbeg a0
        &[0x02, 0xe0, 0x00], // s32c1i
    ];
    for bytes in esp32_only {
        assert!(Isa::ESP32.decode(bytes, 0).is_ok(), "{bytes:02x?}");
        assert_eq!(
            Isa::ESP8266.decode(bytes, 0),
            Err(DecodeError::Invalid),
            "{bytes:02x?}"
        );
    }
}

#[test]
fn esp32s2_is_lx7_without_esp32_options() {
    // salt a3, a4, a5 and get_gpio_in a5 are ESP32-S2 only.
    for bytes in [&[0x50, 0x34, 0x72][..], &[0x50, 0x30, 0x06]] {
        assert!(Isa::ESP32S2.decode(bytes, 0).is_ok(), "{bytes:02x?}");
        assert_eq!(
            Isa::ESP32.decode(bytes, 0),
            Err(DecodeError::Invalid),
            "{bytes:02x?}"
        );
    }
    assert_eq!(
        decode(Isa::ESP32S2, &[0x50, 0x34, 0x72], 0).to_string(),
        "salt a3, a4, a5"
    );
    // No FPU, loops, MAC16 or booleans; windowed calls remain.
    for bytes in [
        &[0x00, 0x34, 0x0a][..],
        &[0x76, 0x80, 0x05],
        &[0x04, 0x00, 0x74],
        &[0x76, 0x00, 0x05],
    ] {
        assert_eq!(
            Isa::ESP32S2.decode(bytes, 0),
            Err(DecodeError::Invalid),
            "{bytes:02x?}"
        );
    }
    assert_eq!(
        decode(Isa::ESP32S2, &[0x36, 0x41, 0x00], 0).opcode(),
        Opcode::Entry
    );
}

#[test]
fn esp32s3_four_byte_format() {
    // ee.vmulas.s16.accx.ld.ip.qup q0, a0, 0, q0, q0, q0, q0
    let bytes = [0x0e, 0x00, 0x00, 0x00];
    assert_eq!(Isa::ESP32S3.instruction_length(0x0e), Some(4));
    assert_eq!(Isa::ESP32S3.instruction_length(0x1f), Some(4));
    assert_eq!(Isa::ESP32.instruction_length(0x0e), None);
    let insn = decode(Isa::ESP32S3, &bytes, 0);
    assert_eq!(insn.len(), 4);
    assert_eq!(insn.opcode(), Opcode::EeVmulasS16AccxLdIpQup);
    assert_eq!(insn.operands().len(), 7);
    assert_eq!(insn.operands()[0], Operand::Qr(0));
    assert_eq!(insn.next_address(), 4);
    assert_eq!(
        Isa::ESP32S3.decode(&bytes[..3], 0),
        Err(DecodeError::Truncated)
    );
    assert_eq!(Isa::ESP32.decode(&bytes, 0), Err(DecodeError::Invalid));
    // The 4-byte nop.
    let nop = decode(Isa::ESP32S3, &0xe601_000e_u32.to_le_bytes(), 0);
    assert_eq!(
        (nop.opcode(), nop.len(), nop.to_string()),
        (Opcode::Nop, 4, "nop".into())
    );
}

#[test]
fn esp32s3_three_byte_pie_and_options() {
    // ld.qr q1, a2, 32 (in the MAC16 opcode space) and mul.aa.ll a2, a3.
    let ld = decode(Isa::ESP32S3, &[0x24, 0xa2, 0xcd], 0);
    assert_eq!(ld.to_string(), "ld.qr q1, a2, 32");
    assert_eq!(
        ld.operands(),
        [Operand::Qr(1), Operand::Ar(2), Operand::Imm(32)]
    );
    assert_eq!(
        Isa::ESP32.decode(&[0x24, 0xa2, 0xcd], 0),
        Err(DecodeError::Invalid)
    );
    assert_eq!(
        decode(Isa::ESP32S3, &[0x34, 0x02, 0x74], 0).opcode(),
        Opcode::MulAaLl
    );
    // FPU and loops remain; the ESP32 f64 assist does not.
    assert!(Isa::ESP32S3.decode(&[0x00, 0x34, 0x0a], 0).is_ok());
    assert!(Isa::ESP32S3.decode(&[0x76, 0x80, 0x05], 0).is_ok());
    assert!(Isa::ESP32.decode(&[0x50, 0x34, 0xeb], 0).is_ok()); // f64cmpl
    assert_eq!(
        Isa::ESP32S3.decode(&[0x50, 0x34, 0xeb], 0),
        Err(DecodeError::Invalid)
    );
}

#[test]
fn register_file_instructions_need_their_option() {
    let without = |isa: Isa, f| Isa {
        features: isa.features.difference(f),
        ..isa
    };
    // ee.ldf.128.ip f0, f0, f0, f0, a0, 0 uses FPU registers.
    let ldf = 0x8000_000e_u32.to_le_bytes();
    assert!(Isa::ESP32S3.decode(&ldf, 0).is_ok());
    assert_eq!(
        without(Isa::ESP32S3, Features::FP).decode(&ldf, 0),
        Err(DecodeError::Invalid)
    );
    // PIE instructions without FPU operands are unaffected.
    let vmulas = [0x0e, 0x00, 0x00, 0x00];
    assert!(
        without(Isa::ESP32S3, Features::FP)
            .decode(&vmulas, 0)
            .is_ok()
    );
    // un.s b3, f4, f5 and movt.s f3, f4, b5 use boolean registers.
    for bytes in [[0x50, 0x34, 0x1b], [0x50, 0x34, 0xdb]] {
        assert!(Isa::ESP32.decode(&bytes, 0).is_ok());
        let no_bool = without(Isa::ESP32, Features::BOOLEAN);
        assert_eq!(
            no_bool.decode(&bytes, 0),
            Err(DecodeError::Invalid),
            "{bytes:02x?}"
        );
    }
}

/// Every register file belongs to an option, so an instruction with an
/// operand in that file must be rejected when the option is removed.
#[test]
fn register_files_require_their_option() {
    let three_byte = (0..1u32 << 24).map(u32::to_le_bytes);
    let four_byte = (0..1u32 << 29)
        .step_by(251)
        .map(|i| ((i >> 1) << 4 | 0xe | (i & 1)).to_le_bytes());
    let words: Vec<[u8; 4]> = three_byte.chain(four_byte).collect();
    for isa in [Isa::ESP32, Isa::ESP32S3] {
        let without = |f| Isa {
            features: isa.features.difference(f),
            ..isa
        };
        let (no_fp, no_bool, no_mac16, no_pie) = (
            without(Features::FP),
            without(Features::BOOLEAN),
            without(Features::MAC16),
            without(Features::ESP32S3_PIE),
        );
        for bytes in &words {
            let Ok(insn) = isa.decode(bytes, 0) else {
                continue;
            };
            for op in insn.operands() {
                let reduced = match op {
                    Operand::Fr(_) => &no_fp,
                    Operand::Br(_) | Operand::BrGroup { .. } => &no_bool,
                    Operand::Mr(_) => &no_mac16,
                    Operand::Qr(_) => &no_pie,
                    _ => continue,
                };
                assert!(
                    reduced.decode(bytes, 0).is_err(),
                    "{insn} decodes without the option owning {op:?}"
                );
            }
        }
    }
}

#[test]
fn feature_gating_is_per_feature() {
    let no_fp = Isa {
        features: Isa::ESP32.features.difference(Features::FP),
        ..Isa::ESP32
    };
    assert!(no_fp.decode(&[0x00, 0x34, 0x0a], 0).is_err());
    assert!(no_fp.decode(&[0x36, 0x41, 0x00], 0).is_ok());
}

#[test]
fn operands_are_structured() {
    // l32i a3, a4, 8
    let insn = decode(Isa::ESP32, &[0x32, 0x24, 0x02], 0);
    assert_eq!(insn.opcode(), Opcode::L32i);
    assert_eq!(
        insn.operands(),
        [Operand::Ar(3), Operand::Ar(4), Operand::Imm(8)]
    );

    // rsr.ccount a2
    let insn = decode(Isa::ESP32, &[0x20, 0xea, 0x03], 0);
    assert_eq!(insn.opcode(), Opcode::Rsr);
    assert_eq!(insn.to_string(), "rsr.ccount a2");
    assert_eq!(insn.mnemonic().to_string(), "rsr.ccount");
    assert_eq!(insn.text_operands(), [Operand::Ar(2)]);
    let Operand::Sr(sr) = insn.operands()[1] else {
        panic!()
    };
    assert_eq!((sr.number, sr.name), (234, "ccount"));

    // any4 b0, b4:b5:b6:b7
    let insn = decode(Isa::ESP32, &[0x00, 0x84, 0x00], 0);
    assert_eq!(insn.operands()[1], Operand::BrGroup { first: 4, count: 4 });
    assert_eq!(insn.to_string(), "any4 b0, b4:b5:b6:b7");

    // movi a2, -2048 prints as hex like objdump.
    let insn = decode(Isa::ESP32, &[0x22, 0xa8, 0x00], 0);
    assert_eq!(insn.operands()[1], Operand::Imm(-2048));
    assert_eq!(insn.to_string(), "movi a2, 0xfffff800");
}

#[test]
fn branch_targets() {
    let base = 0x4000_1000;
    // beqz.n a2, +4 (forward only)
    let insn = decode(Isa::ESP32, &[0x8c, 0x02], base);
    assert_eq!(
        insn.control_flow(),
        ControlFlow::Branch { target: base + 4 }
    );
    // bne a2, a3, -4 (to itself + 0)
    let insn = decode(Isa::ESP32, &[0x37, 0x92, 0xfc], base);
    assert_eq!(insn.control_flow(), ControlFlow::Branch { target: base });
    // j -4
    let insn = decode(Isa::ESP32, &[0x06, 0xff, 0xff], base);
    assert_eq!(insn.control_flow(), ControlFlow::Jump { target: base });
    assert_eq!(insn.to_string(), "j 0x40001000");
    // call8 with offset 0 from an unaligned address targets the next word.
    let insn = decode(Isa::ESP32, &[0x25, 0x00, 0x00], base + 3);
    assert_eq!(
        insn.control_flow(),
        ControlFlow::Call {
            target: base + 4,
            window: 8
        }
    );
    // l32r a2, <base+3 rounded up - 4>
    let insn = decode(Isa::ESP32, &[0x21, 0xff, 0xff], base + 1);
    assert_eq!(insn.operands()[1], Operand::Literal(base));
    // loop a2, end = pc + 4 + imm8
    let insn = decode(Isa::ESP32, &[0x76, 0x82, 0x10], base);
    assert_eq!(
        insn.control_flow(),
        ControlFlow::Loop {
            end: base + 0x14,
            conditional: false
        }
    );
}

#[test]
fn targets_wrap_around_the_address_space() {
    let insn = decode(Isa::ESP32, &[0x06, 0xff, 0xff], 0);
    assert_eq!(insn.control_flow(), ControlFlow::Jump { target: 0 });
    let insn = decode(Isa::ESP32, &[0x06, 0x00, 0x00], 0xffff_fffe);
    assert_eq!(insn.control_flow(), ControlFlow::Jump { target: 2 });
}

#[test]
fn control_flow_classes() {
    let cases: &[(&[u8], ControlFlow)] = &[
        (&[0x80, 0x00, 0x00], ControlFlow::Return),       // ret
        (&[0x0d, 0xf0], ControlFlow::Return),             // ret.n
        (&[0x90, 0x00, 0x00], ControlFlow::Return),       // retw
        (&[0xa0, 0x02, 0x00], ControlFlow::IndirectJump), // jx a2
        (&[0xc0, 0x02, 0x00], ControlFlow::IndirectCall { window: 0 }), // callx0 a2
        (&[0xe0, 0x08, 0x00], ControlFlow::IndirectCall { window: 8 }), // callx8 a8
        (&[0x00, 0x30, 0x00], ControlFlow::ExceptionReturn), // rfe
        (&[0x10, 0x33, 0x00], ControlFlow::ExceptionReturn), // rfi 3
        (&[0x00, 0x50, 0x00], ControlFlow::Trap),         // syscall
        (&[0x6d, 0xf0], ControlFlow::Trap),               // ill.n
        (&[0x00, 0x41, 0x00], ControlFlow::Trap),         // break 1, 0
        (&[0x3d, 0xf0], ControlFlow::Sequential),         // nop.n
        (&[0x20, 0xea, 0x03], ControlFlow::Sequential),   // rsr.ccount a2
    ];
    for (bytes, flow) in cases {
        assert_eq!(
            decode(Isa::ESP32, bytes, 0).control_flow(),
            *flow,
            "{bytes:02x?}"
        );
    }
    let insn = decode(Isa::ESP32, &[0x76, 0x92, 0x00], 0); // loopnez
    assert_eq!(
        insn.control_flow(),
        ControlFlow::Loop {
            end: 4,
            conditional: true
        }
    );
}

#[test]
fn every_branch_like_opcode_reports_its_target() {
    // Exercise control_flow() on every encoding with a target operand so a
    // missing match arm would panic or misclassify.
    for isa in [Isa::ESP32, Isa::ESP32S2, Isa::ESP32S3, Isa::ESP8266] {
        for word in (0..1u32 << 24).step_by(97) {
            let Ok(insn) = isa.decode(&word.to_le_bytes()[..3], 0x1000) else {
                continue;
            };
            let flow = insn.control_flow();
            match insn.operands().last() {
                Some(Operand::Target(t)) => assert!(
                    matches!(flow, ControlFlow::Branch { target } | ControlFlow::Jump { target }
                        | ControlFlow::Call { target, .. } if target == *t)
                        || matches!(flow, ControlFlow::Loop { end, .. } if end == *t),
                    "{insn}: {flow:?}"
                ),
                _ => assert!(
                    !matches!(
                        flow,
                        ControlFlow::Branch { .. }
                            | ControlFlow::Jump { .. }
                            | ControlFlow::Call { .. }
                            | ControlFlow::Loop { .. }
                    ),
                    "{insn}: {flow:?}"
                ),
            }
        }
    }
}

#[test]
fn disassembly_iterator() {
    // entry a1, 32 ; <invalid 0xff> ; retw.n ; truncated l32i
    let code = [0x36, 0x41, 0x00, 0xff, 0x1d, 0xf0, 0x32, 0x24];
    let items: Vec<_> = Isa::ESP32.disassemble(&code, 0x100).collect();
    let summary: Vec<_> = items
        .iter()
        .map(|(addr, r)| (*addr, r.map(|insn| insn.opcode())))
        .collect();
    assert_eq!(
        summary,
        [
            (0x100, Ok(Opcode::Entry)),
            (0x103, Err(DecodeError::Invalid)),
            (0x104, Ok(Opcode::RetwN)),
            (0x106, Err(DecodeError::Truncated)),
        ]
    );
}

#[test]
fn mnemonics_are_unique_and_lowercase() {
    let mut seen = std::collections::HashSet::new();
    for op in Opcode::ALL {
        let m = op.mnemonic();
        assert!(seen.insert(m), "duplicate mnemonic {m}");
        assert_eq!(m, m.to_lowercase());
        assert_eq!(op.to_string(), m);
    }
}

#[test]
fn every_opcode_is_reachable() {
    let mut seen = std::collections::HashSet::new();
    for isa in [Isa::ESP32, Isa::ESP32S2, Isa::ESP32S3] {
        for word in 0..1u32 << 24 {
            if let Ok(insn) = isa.decode(&word.to_le_bytes()[..3], 0) {
                seen.insert(insn.opcode());
            }
        }
    }
    // The 4-byte format: a dense sample reaches every instruction.
    for word in (0..1u32 << 29).step_by(61) {
        let word = (word >> 1) << 4 | 0xe | (word & 1);
        if let Ok(insn) = Isa::ESP32S3.decode(&word.to_le_bytes(), 0) {
            seen.insert(insn.opcode());
        }
    }
    let missing: Vec<_> = Opcode::ALL.iter().filter(|op| !seen.contains(op)).collect();
    assert!(missing.is_empty(), "unreachable opcodes: {missing:?}");
}
