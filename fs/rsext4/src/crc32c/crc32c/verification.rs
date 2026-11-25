//! Kani harnesses for the software CRC32C: its one-byte step and every entry of its slicing
//! tables equal the bitwise definition. Behind `#[cfg(kani)]`, so only `cargo kani` compiles them.
//!
//! The harnesses call `crc32c_update`, the software path, directly. `crc32c_append` takes the
//! hardware path when the CPU reports it, and that path is one instruction, not code a prover
//! reads. The polynomial below follows Request for Comments (RFC) 3720 appendix B.4 rather than
//! the module, so a wrong constant cannot prove itself right.

use super::*;

/// The CRC32C polynomial, reflected, as RFC 3720 appendix B.4 defines it.
const CRC32C_POLYNOMIAL_REFLECTED: u32 = 0x82F6_3B78;

/// The bitwise definition of a raw CRC32C update: no table, one bit at a time.
fn crc32c_bitwise(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (CRC32C_POLYNOMIAL_REFLECTED & mask);
        }
    }
    crc
}

/// **The one-byte step equals the bitwise definition for every seed and every byte**, and
/// `crc32c_init` and `crc32c_finalize` are the standard initial and final values. A buffer
/// shorter than eight bytes, and the tail of every longer one, goes through this step.
#[kani::proof]
fn software_crc32c_tail_step_equals_bitwise_definition() {
    let data: [u8; 1] = kani::any();
    let seed: u32 = kani::any();
    let table = crc32c_update(seed, &data);
    assert_eq!(table, crc32c_bitwise(seed, &data));
    assert_eq!(crc32c_finalize(table), !table);
    assert_eq!(crc32c_init(), u32::MAX);
}

/// **Every entry of the eight slicing tables equals the bitwise definition.** An eight-byte round
/// over a zero seed and a word that is zero but for one arbitrary byte at an arbitrary position
/// reads exactly one entry of one table, and it equals the bitwise CRC32C of that word.
///
/// What this does not prove: that a whole round with an arbitrary seed and word equals the
/// bitwise definition. That follows from the entries because CRC32C without its initial and final
/// values is linear over the field of two elements, so a round is the exclusive or of one entry
/// per byte. Kani does not check that step, because the solver does not finish the whole round in
/// fifteen minutes.
#[kani::proof]
#[kani::unwind(9)]
fn software_crc32c_tables_equal_bitwise_definition() {
    let position: usize = kani::any();
    kani::assume(position < 8);
    let mut data = [0u8; 8];
    data[position] = kani::any();
    assert_eq!(crc32c_update(0, &data), crc32c_bitwise(0, &data));
}
