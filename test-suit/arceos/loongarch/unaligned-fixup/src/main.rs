#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use ax_cpu as _;

#[cfg(feature = "ax-std")]
const UNMAPPED_ADDRESS: u64 = 0x1000;
#[cfg(feature = "ax-std")]
const ACCESS_SIZE: u64 = 8;

#[cfg(feature = "ax-std")]
unsafe extern "C" {
    fn _unaligned_read(
        address: u64,
        value: &mut u64,
        size: u64,
        signed: bool,
        fault_address: &mut u64,
    ) -> i32;
    fn _unaligned_write(address: u64, value: u64, size: u64, fault_address: &mut u64) -> i32;
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
#[cfg(feature = "ax-std")]
fn main() {
    let mut value = u64::MAX;
    let mut read_fault_address = 0;

    // SAFETY: the ArceOS runtime has installed the LoongArch exception table.
    // The intentionally unmapped address must be recovered by the helper's
    // fixup instead of escaping this call.
    let read_result = unsafe {
        _unaligned_read(
            UNMAPPED_ADDRESS,
            &mut value,
            ACCESS_SIZE,
            false,
            &mut read_fault_address,
        )
    };
    assert_eq!(read_result, -1);
    assert_eq!(read_fault_address, UNMAPPED_ADDRESS + ACCESS_SIZE - 1);
    assert_eq!(value, u64::MAX);

    let mut write_fault_address = 0;
    // SAFETY: as above, the exception-table fixup owns recovery from the
    // deliberate store fault and writes the failed byte address to the output.
    let write_result = unsafe {
        _unaligned_write(
            UNMAPPED_ADDRESS,
            0x0123_4567_89ab_cdef,
            ACCESS_SIZE,
            &mut write_fault_address,
        )
    };
    assert_eq!(write_result, -1);
    assert_eq!(write_fault_address, UNMAPPED_ADDRESS);

    std::println!("LOONGARCH_UNALIGNED_FIXUP_OK");
    ax_hal::power::system_off();
}

#[cfg(not(feature = "ax-std"))]
fn main() {
    eprintln!("this target requires the ax-std feature");
}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
