#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

extern crate alloc;

#[cfg(feature = "ax-std")]
extern crate ax_std;

use arceos_axtest_sg2002_usb_msc::usb as sg2002_usb_msc;

#[cfg(all(feature = "ax-std", axtest))]
#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    #[test]
    fn sg2002_dwc2_usb_msc_read_smoke() -> axtest::AxTestResult {
        match ax_runtime::task::executor::block_on(super::sg2002_usb_msc::run()) {
            Ok(report) => {
                axtest_println!(
                    "SG2002_DWC2_MSC_REPORT vid={:04x} pid={:04x} vendor=\"{}\" product=\"{}\" \
                     blocks={} block_size={} checksum=0x{:08x}",
                    report.vendor_id,
                    report.product_id,
                    report.inquiry_vendor,
                    report.inquiry_product,
                    report.blocks,
                    report.block_size,
                    report.read_checksum
                );
                axtest::AxTestResult::Ok
            }
            Err(err) => {
                axtest_println!("SG2002_DWC2_MSC_FAIL error={err}");
                axtest::AxTestResult::Failed
            }
        }
    }
}

#[cfg(not(feature = "ax-std"))]
fn main() {
    eprintln!("arceos-axtest-sg2002-usb-msc is only meaningful as an ArceOS axtest target");
}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
