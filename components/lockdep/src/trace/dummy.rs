#[cfg(any(test, doctest))]
pub(super) fn emit_byte(byte: u8) {
    std::eprint!("{}", byte as char);
}

#[cfg(all(not(test), not(doctest), not(target_arch = "riscv64")))]
pub(super) fn emit_byte(_byte: u8) {}
