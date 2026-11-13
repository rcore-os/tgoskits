use arceos_axtest_sg2002_usb_msc::usb;
use ax_std as _;

fn main() {
    println!("SG2002 USB MSC manual example: connect a readable USB storage device");
    match ax_runtime::task::executor::block_on(usb::run()) {
        Ok(report) => {
            println!(
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
            println!("SG2002_USB_MSC_APP_OK");
        }
        Err(error) => {
            eprintln!("SG2002_DWC2_MSC_FAIL error={error}");
            eprintln!(
                "Check the board, USB storage connection and the last completed stage above."
            );
            std::process::exit(1);
        }
    }
}
