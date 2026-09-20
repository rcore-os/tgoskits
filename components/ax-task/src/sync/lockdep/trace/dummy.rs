#[cfg(any(test, doctest, all(feature = "host-test", not(target_os = "none"))))]
pub(super) fn emit_byte(byte: u8) {
    std::eprint!("{}", byte as char);
}

#[cfg(all(
    not(any(test, doctest, all(feature = "host-test", not(target_os = "none")))),
    not(target_arch = "riscv64")
))]
pub(super) fn emit_byte(_byte: u8) {}
