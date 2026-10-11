//! Compares the decoder against GNU objdump for every encoding.
//!
//! Reads the output of `scripts/oracle/sweep.py` on stdin and reports each
//! encoding where the decoder disagrees with objdump. If there are none, it
//! prints a digest of the decoder's output to record in
//! `tests/data/<isa>.digest`, which `tests/exhaustive.rs` checks:
//!
//! ```text
//! scripts/oracle/sweep.py target/oracle/xtensa-esp-elf esp32 \
//!     | cargo run --release --example oracle_diff -- esp32
//! ```
//!
//! The input must be the complete sweep in its canonical order; anything
//! else (missing, extra, duplicated or reordered lines) fails. Pass
//! `--partial` to compare an arbitrary subset, e.g. a fixture file, without
//! producing a digest.
//!
//! The input is split into the sweep's chunks, which worker threads check in
//! parallel; results are reassembled in order, so the output is
//! deterministic.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::sync::{Mutex, mpsc};
use std::thread;

use xtensa::Isa;

#[allow(dead_code)]
#[path = "../tests/common/digest.rs"]
mod digest;
#[path = "../tests/common/objdump.rs"]
mod objdump;

/// Mismatches printed in total.
const MAX_REPORTED: usize = 100;

struct ChunkResult {
    lines: u64,
    mismatches: u64,
    /// The first few mismatches, for the report.
    diffs: Vec<String>,
    /// The chunk's digest, if it is exactly the expected sweep chunk.
    digest: Option<u64>,
}

fn check_chunk(isa: &Isa, index: u32, text: &str) -> ChunkResult {
    let mut expected = (index < digest::chunk_count(isa)).then(|| digest::chunk(index));
    let mut in_order = expected.is_some();
    let (mut hasher, mut scratch) = (digest::Fnv::new(), String::new());
    let mut result = ChunkResult {
        lines: 0,
        mismatches: 0,
        diffs: Vec::new(),
        digest: None,
    };
    for line in text.lines() {
        let line = objdump::Line::parse(line);
        result.lines += 1;
        in_order &= expected.as_mut().and_then(Iterator::next) == Some((line.word, line.address));
        if in_order {
            digest::add(&mut hasher, &mut scratch, isa, line.word, line.address);
        }
        if let Err(diff) = objdump::compare(isa, &line) {
            result.mismatches += 1;
            if result.diffs.len() < MAX_REPORTED {
                result.diffs.push(diff);
            }
        }
    }
    let complete = expected.is_some_and(|mut rest| rest.next().is_none());
    if in_order && complete {
        result.digest = Some(hasher.finish());
    }
    result
}

/// Reads the next chunk's lines, sized as the sweep's chunk `index`.
fn read_chunk(input: &mut impl BufRead, isa: &Isa, index: u32) -> String {
    let lines = if index < digest::chunk_count(isa) {
        digest::chunk(index).count()
    } else {
        1 << 16
    };
    let mut text = String::new();
    for _ in 0..lines {
        if input.read_line(&mut text).expect("read stdin") == 0 {
            break;
        }
    }
    text
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (name, partial) = match args.as_slice() {
        [name] => (name.as_str(), false),
        [name, flag] if flag == "--partial" => (name.as_str(), true),
        _ => (Default::default(), false),
    };
    let Some(isa) = objdump::isa_by_name(name) else {
        eprintln!("usage: oracle_diff <esp32|esp32s2|esp32s3|esp8266> [--partial]");
        return ExitCode::FAILURE;
    };
    let workers = thread::available_parallelism().map_or(1, std::num::NonZero::get);

    // Bounded so the reader stays only a little ahead of the workers.
    let (job_tx, job_rx) = mpsc::sync_channel::<(u32, String)>(workers * 2);
    let job_rx = Mutex::new(job_rx);
    let (result_tx, result_rx) = mpsc::channel();
    let mut results = Vec::new();
    thread::scope(|scope| {
        for _ in 0..workers {
            let (job_rx, result_tx, isa) = (&job_rx, result_tx.clone(), &isa);
            scope.spawn(move || {
                loop {
                    let job = job_rx.lock().unwrap().recv();
                    let Ok((index, text)) = job else { break };
                    result_tx
                        .send((index, check_chunk(isa, index, &text)))
                        .unwrap();
                }
            });
        }
        drop(result_tx);

        let mut input = io::stdin().lock();
        for index in 0.. {
            let text = read_chunk(&mut input, &isa, index);
            if text.is_empty() {
                break;
            }
            job_tx.send((index, text)).unwrap();
        }
        drop(job_tx);
        results.extend(result_rx);
    });
    results.sort_by_key(|(index, _)| *index);

    let mut out = io::stdout().lock();
    let total: u64 = results.iter().map(|(_, r)| r.lines).sum();
    let mismatches: u64 = results.iter().map(|(_, r)| r.mismatches).sum();
    for diff in results
        .iter()
        .flat_map(|(_, r)| &r.diffs)
        .take(MAX_REPORTED)
    {
        writeln!(out, "{diff}").unwrap();
    }
    writeln!(out, "{name}: {total} encodings, {mismatches} mismatches").unwrap();
    if total == 0 || mismatches != 0 {
        return ExitCode::FAILURE;
    }
    if partial {
        return ExitCode::SUCCESS;
    }
    // Only a complete sweep in the canonical order verifies every encoding,
    // and only it yields a digest.
    let digests: Option<Vec<u64>> = results.iter().map(|(_, r)| r.digest).collect();
    if let Some(digests) = digests.filter(|d| d.len() == digest::chunk_count(&isa) as usize) {
        writeln!(out, "digest: {:016x}", digest::combine(digests)).unwrap();
        ExitCode::SUCCESS
    } else {
        writeln!(
            out,
            "{name}: input is not the complete sweep (use --partial for subsets)"
        )
        .unwrap();
        ExitCode::FAILURE
    }
}
