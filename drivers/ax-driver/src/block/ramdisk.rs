use rdrive::PlatformDevice;

use crate::block::PlatformInlineBlock;

pub const BLOCK_SIZE: usize = 512;
pub const DEFAULT_SIZE: usize = 16 * 1024 * 1024;

pub const DEVICE_NAME: &str = "ramdisk";

pub fn register(plat_dev: PlatformDevice) {
    let blocks = DEFAULT_SIZE / BLOCK_SIZE;
    let device =
        match ramdisk::RamDisk::with_name(DEVICE_NAME, BLOCK_SIZE, blocks).into_inline_device() {
            Ok(device) => device,
            Err(error) => {
                log::error!("refusing invalid ramdisk configuration: {error}");
                return;
            }
        };
    let registration = plat_dev.register_inline_block(device);
    log::info!(
        "registered inline ramdisk: {} bytes, slot={registration:?}",
        DEFAULT_SIZE
    );
}
