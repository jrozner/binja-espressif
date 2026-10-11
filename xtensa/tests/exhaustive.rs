//! Decodes every instruction encoding and checks the result against a digest
//! recorded by `examples/oracle_diff.rs` after it verified every one of them
//! against GNU objdump. A mismatch means the decoder's output changed
//! somewhere; rerun the oracle comparison to find out where and whether the
//! change is correct.
//!
//! These tests take many CPU-minutes, so CI skips them on pushes and pull
//! requests and runs them nightly (`.github/workflows/nightly.yml`).

mod common;

use common::digest::digest;
use xtensa::Isa;

fn check(isa: &Isa, recorded: &str) {
    assert_eq!(format!("{:016x}", digest(isa)), recorded.trim());
}

#[test]
fn esp32_exhaustive_digest() {
    check(&Isa::ESP32, include_str!("data/esp32.digest"));
}

#[test]
fn esp32s2_exhaustive_digest() {
    check(&Isa::ESP32S2, include_str!("data/esp32s2.digest"));
}

/// Covers the 4-byte format's 2^29 encodings in addition to the usual ones.
#[test]
fn esp32s3_exhaustive_digest() {
    check(&Isa::ESP32S3, include_str!("data/esp32s3.digest"));
}

#[test]
fn esp8266_exhaustive_digest() {
    check(&Isa::ESP8266, include_str!("data/esp8266.digest"));
}
