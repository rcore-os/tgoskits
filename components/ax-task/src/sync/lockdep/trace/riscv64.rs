#[allow(deprecated)]
pub(super) fn emit_byte(byte: u8) {
    sbi_rt::legacy::console_putchar(byte as usize);
}
