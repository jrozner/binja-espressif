//! A digest of the decoder's output over every instruction encoding.
//!
//! `examples/oracle_diff.rs` prints this digest after verifying every
//! encoding against objdump, and `tests/exhaustive.rs` checks the decoder
//! still produces it, so `cargo test` covers the whole encoding space without
//! needing the toolchain.
//!
//! Encodings are grouped into the chunks of `scripts/oracle/sweep.py`. The
//! digest is a hash of the per-chunk hashes, so chunks can be hashed in
//! parallel.

use std::fmt::Write;

use xtensa::{ControlFlow, Instruction, Isa, Operand};

const CHUNK: u32 = 1 << 16;
const WIDE_CHUNKS: u32 = 256;
const NARROW_CHUNK: u32 = WIDE_CHUNKS;
const FOUR_BYTE_CHUNKS: u32 = (1 << 29) / CHUNK;

/// The number of chunks `sweep.py` produces for `isa`.
pub fn chunk_count(isa: &Isa) -> u32 {
    let four_byte = isa.instruction_length(0x0e) == Some(4);
    NARROW_CHUNK + 1 + if four_byte { FOUR_BYTE_CHUNKS } else { 0 }
}

/// The `(word, address)` pairs of a chunk, in `sweep.py` order: all 3-byte
/// encodings (op0 < 8) by their high byte, then all 16-bit words with
/// op0 >= 8, then (for chips with the 4-byte format) all 32-bit words whose
/// bits [3:1] are set.
#[allow(clippy::cast_possible_truncation)] // Indices are below 2^16.
pub fn chunk(chunk: u32) -> Box<dyn Iterator<Item = (u32, u32)>> {
    let with_addresses = |words: Box<dyn Iterator<Item = u32>>| {
        Box::new(words.enumerate().map(|(i, w)| (w, i as u32 * 4)))
    };
    if chunk < WIDE_CHUNKS {
        with_addresses(Box::new(
            (0..CHUNK)
                .filter(|v| v & 0xf < 8)
                .map(move |v| v | chunk << 16),
        ))
    } else if chunk == NARROW_CHUNK {
        with_addresses(Box::new((0..CHUNK).filter(|v| v & 0xf >= 8)))
    } else {
        let base = (chunk - NARROW_CHUNK - 1) * CHUNK;
        with_addresses(Box::new(
            (base..base + CHUNK).map(|i| (i >> 1) << 4 | 0xe | (i & 1)),
        ))
    }
}

/// FNV-1a, which (unlike `DefaultHasher`) is stable across Rust releases.
pub struct Fnv(u64);

impl Fnv {
    pub fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub fn finish(&self) -> u64 {
        self.0
    }
}

/// Renders what an instruction means beyond its text: each operand's kind
/// and value, and its control flow. (Spelled out rather than using `Debug`,
/// whose output is not guaranteed to stay the same.)
fn structure(text: &mut String, insn: &Instruction) {
    for op in insn.operands() {
        match *op {
            Operand::Ar(n) => write!(text, " ar{n}"),
            Operand::Fr(n) => write!(text, " fr{n}"),
            Operand::Br(n) => write!(text, " br{n}"),
            Operand::BrGroup { first, count } => write!(text, " brg{first}/{count}"),
            Operand::Mr(n) => write!(text, " mr{n}"),
            Operand::Qr(n) => write!(text, " qr{n}"),
            Operand::Imm(v) => write!(text, " imm{v}"),
            Operand::Target(a) => write!(text, " target{a:x}"),
            Operand::Literal(a) => write!(text, " literal{a:x}"),
            Operand::Sr(sr) => write!(text, " sr{}", sr.number),
            Operand::Ur(ur) => write!(text, " ur{}", ur.number),
        }
        .unwrap();
    }
    match insn.control_flow() {
        ControlFlow::Sequential => write!(text, " | seq"),
        ControlFlow::Branch { target } => write!(text, " | branch {target:x}"),
        ControlFlow::Jump { target } => write!(text, " | jump {target:x}"),
        ControlFlow::IndirectJump => write!(text, " | jump indirect"),
        ControlFlow::Call { target, window } => write!(text, " | call{window} {target:x}"),
        ControlFlow::IndirectCall { window } => write!(text, " | call{window} indirect"),
        ControlFlow::Return => write!(text, " | return"),
        ControlFlow::ExceptionReturn => write!(text, " | exception return"),
        ControlFlow::Loop { end, conditional } => write!(text, " | loop {end:x} {conditional}"),
        ControlFlow::Trap => write!(text, " | trap"),
    }
    .unwrap();
}

/// Adds one decode result to a chunk's hash: the instruction's length and
/// text (which objdump verified) and its structure (operands and control
/// flow), so changes to either are caught.
pub fn add(hasher: &mut Fnv, text: &mut String, isa: &Isa, word: u32, address: u32) {
    text.clear();
    match isa.decode(&word.to_le_bytes(), address) {
        Ok(insn) => {
            write!(text, "{word:08x} {} {insn} |", insn.len()).unwrap();
            structure(text, &insn);
            text.push('\n');
        }
        Err(_) => writeln!(text, "{word:08x} invalid").unwrap(),
    }
    hasher.write(text.as_bytes());
}

/// The hash of one chunk.
pub fn chunk_digest(isa: &Isa, index: u32) -> u64 {
    let (mut hasher, mut text) = (Fnv::new(), String::new());
    for (word, address) in chunk(index) {
        add(&mut hasher, &mut text, isa, word, address);
    }
    hasher.finish()
}

/// Combines chunk hashes, in order, into the digest.
pub fn combine(chunk_digests: impl IntoIterator<Item = u64>) -> u64 {
    let mut hasher = Fnv::new();
    for digest in chunk_digests {
        hasher.write(&digest.to_le_bytes());
    }
    hasher.finish()
}

/// The digest of every encoding, computed on all available cores.
#[allow(clippy::cast_possible_truncation)] // Chunk and thread counts are small.
pub fn digest(isa: &Isa) -> u64 {
    let count = chunk_count(isa);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let mut digests = vec![0; count as usize];
    std::thread::scope(|scope| {
        for (t, slots) in digests
            .chunks_mut(count.div_ceil(threads) as usize)
            .enumerate()
        {
            let first = t as u32 * count.div_ceil(threads);
            scope.spawn(move || {
                for (i, slot) in slots.iter_mut().enumerate() {
                    *slot = chunk_digest(isa, first + i as u32);
                }
            });
        }
    });
    combine(digests)
}
