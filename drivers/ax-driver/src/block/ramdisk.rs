use rdrive::PlatformDevice;

use crate::block::PlatformDeviceBlock;

pub const BLOCK_SIZE: usize = 512;
pub const DEFAULT_SIZE: usize = 16 * 1024 * 1024;

pub const DEVICE_NAME: &str = "ramdisk";

pub fn register(plat_dev: PlatformDevice) {
    let blocks = DEFAULT_SIZE / BLOCK_SIZE;
    plat_dev.register_block(ramdisk::RamDisk::with_name(DEVICE_NAME, BLOCK_SIZE, blocks));
    log::info!("registered ramdisk: {} bytes", DEFAULT_SIZE);
}
