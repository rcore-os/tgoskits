use memory_addr::VirtAddr;
use sbi_spec::binary::{Physical, SbiRet};

pub const EID_DBCN: usize = 0x4442434e;
pub const FID_CONSOLE_WRITE: usize = 0;
pub const FID_CONSOLE_READ: usize = 1;
pub const FID_CONSOLE_WRITE_BYTE: usize = 2;

/// SBI success state return value.
pub const RET_SUCCESS: usize = 0;
/// Error for SBI call failed for unknown reasons.
pub const RET_ERR_FAILED: usize = -1isize as _;
/// Error for target operation not supported.
pub const RET_ERR_NOT_SUPPORTED: usize = -2isize as _;

/// Writes bytes to the console using SBI byte-wise API.
pub fn console_write(buf: &[u8]) -> SbiRet {
    let ptr = buf.as_ptr();
    sbi_rt::console_write(Physical::new(
        buf.len(),
        axvisor_api::memory::virt_to_phys(VirtAddr::from_ptr_of(ptr)).as_usize(),
        0,
    ))
}

/// Reads bytes from the console into a buffer using SBI byte-wise API.
pub fn console_read(buf: &mut [u8]) -> SbiRet {
    let ptr = buf.as_mut_ptr();
    sbi_rt::console_read(Physical::new(
        buf.len(),
        axvisor_api::memory::virt_to_phys(VirtAddr::from_ptr_of(ptr)).as_usize(),
        0,
    ))
}

/// Writes a full string to console using SBI byte-wise API (no log prefix).
#[inline(always)]
#[allow(dead_code)]
pub fn print_str(s: &str) {
    console_write(s.as_bytes());
}

/// Writes a full string + newline to console (no log prefix).
#[inline(always)]
#[allow(dead_code)]
pub fn println_str(s: &str) {
    print_str(s);
    sbi_rt::console_write_byte(b'\n');
}

/// Writes a byte to the console.
#[inline(always)]
pub fn print_byte(byte: u8) {
    sbi_rt::console_write_byte(byte);
}

/// Joins two `usize` values into a `u64` value representing a guest physical address (GPA).
#[inline(always)]
pub fn join_u64(base_lo: usize, base_hi: usize) -> u64 {
    ((base_hi as u64) << 32) | (base_lo as u64)
}
