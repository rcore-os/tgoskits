#[cfg(feature = "block")]
use alloc::boxed::Box;
use alloc::vec::Vec;
#[cfg(feature = "block")]
use core::ops::Range;

#[cfg(feature = "block")]
use ax_driver_base::{BaseDriverOps, DevError, DeviceType};
#[cfg(feature = "block")]
use ax_driver_block::{
    BlockDriverOps,
    gpt::{GptPartitionDev, find_partition_range, is_gpt_disk, list_partitions},
};

#[cfg(feature = "block")]
struct DynBlock(Box<dyn BlockDriverOps>);

#[cfg(feature = "block")]
impl BaseDriverOps for DynBlock {
    fn device_name(&self) -> &str {
        self.0.device_name()
    }

    fn device_type(&self) -> DeviceType {
        self.0.device_type()
    }

    fn irq_num(&self) -> Option<usize> {
        self.0.irq_num()
    }
}

#[cfg(feature = "block")]
impl BlockDriverOps for DynBlock {
    fn num_blocks(&self) -> u64 {
        self.0.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.0.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), DevError> {
        self.0.read_block(block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> Result<(), DevError> {
        self.0.write_block(block_id, buf)
    }

    fn flush(&mut self) -> Result<(), DevError> {
        self.0.flush()
    }
}

#[cfg(feature = "block")]
const EXT4_SUPERBLOCK_OFFSET: usize = 1024;
#[cfg(feature = "block")]
const EXT4_SUPERBLOCK_MAGIC_OFFSET: usize = 0x38;
#[cfg(feature = "block")]
const EXT4_SUPERBLOCK_MAGIC: u16 = 0xEF53;

#[cfg(feature = "block")]
fn partition_has_ext4<T: BlockDriverOps>(
    dev: &mut T,
    range: &Range<u64>,
) -> Result<bool, DevError> {
    let block_size = dev.block_size();
    if block_size == 0 {
        return Err(DevError::InvalidParam);
    }

    let magic_offset = EXT4_SUPERBLOCK_OFFSET + EXT4_SUPERBLOCK_MAGIC_OFFSET;
    let block_index = magic_offset / block_size;
    let within_block = magic_offset % block_size;
    if within_block + 2 > block_size {
        return Err(DevError::InvalidParam);
    }

    let block_id = range
        .start
        .checked_add(u64::try_from(block_index).map_err(|_| DevError::BadState)?)
        .ok_or(DevError::BadState)?;
    if block_id >= range.end {
        return Ok(false);
    }

    let mut buf = alloc::vec![0u8; block_size];
    dev.read_block(block_id, &mut buf)?;
    let magic = u16::from_le_bytes([buf[within_block], buf[within_block + 1]]);
    Ok(magic == EXT4_SUPERBLOCK_MAGIC)
}

pub fn probe_all_devices() -> Vec<super::AxDeviceEnum> {
    #[cfg(target_os = "none")]
    {
        if let Err(err) = axplat_dyn::drivers::probe_all_devices() {
            error!("failed to probe dynamic platform devices: {err:?}");
            return Vec::new();
        }

        #[allow(unused_mut)]
        let mut devices = Vec::new();

        #[cfg(feature = "block")]
        for dev in axplat_dyn::drivers::take_block_devices() {
            let mut raw = DynBlock(dev);

            match is_gpt_disk(&mut raw) {
                Ok(true) => {
                    let root = "rootfs".parse().unwrap();
                    match find_partition_range(&mut raw, |_, part| part.name == root) {
                        Ok(Some(range)) => {
                            info!(
                                "using GPT partition 'rootfs' on block device {}: lba {}..{}",
                                raw.device_name(),
                                range.start,
                                range.end
                            );
                            devices.push(super::AxDeviceEnum::Block(Box::new(
                                GptPartitionDev::new(raw, range),
                            )));
                        }
                        Ok(None) => {
                            match list_partitions(&mut raw) {
                                Ok(partitions) => {
                                    let mut selected = None;
                                    for partition in partitions {
                                        match partition_has_ext4(&mut raw, &partition.range) {
                                            Ok(true) => {
                                                selected = Some(partition);
                                                break;
                                            }
                                            Ok(false) => {}
                                            Err(err) => {
                                                warn!(
                                                    "failed to inspect filesystem signature on partition '{}' of block device {}: {err}",
                                                    partition.entry.name,
                                                    raw.device_name()
                                                );
                                            }
                                        }
                                    }

                                    if let Some(partition) = selected {
                                        let range = partition.range;
                                        info!(
                                            "using ext4 GPT partition '{}' on block device {}: lba {}..{}",
                                            partition.entry.name,
                                            raw.device_name(),
                                            range.start,
                                            range.end
                                        );
                                        devices.push(super::AxDeviceEnum::Block(Box::new(
                                            GptPartitionDev::new(raw, range),
                                        )));
                                    } else {
                                        warn!(
                                            "GPT detected on block device {}, but no partition named 'rootfs' or ext4 partition was found; using raw device",
                                            raw.device_name()
                                        );
                                        devices.push(super::AxDeviceEnum::Block(Box::new(raw)));
                                    }
                                }
                                Err(err) => {
                                    info!(
                                        "failed to inspect GPT partitions on block device {}; using raw device: {err}",
                                        raw.device_name()
                                    );
                                    devices.push(super::AxDeviceEnum::Block(Box::new(raw)));
                                }
                            }
                        }
                        Err(err) => {
                            warn!(
                                "failed to inspect GPT partitions on block device {}; using raw \
                                 device: {err}",
                                raw.device_name()
                            );
                            devices.push(super::AxDeviceEnum::Block(Box::new(raw)));
                        }
                    }
                }
                Ok(false) => devices.push(super::AxDeviceEnum::Block(Box::new(raw))),
                Err(err) => {
                    warn!(
                        "failed to probe GPT on block device {}; using raw device: {err}",
                        raw.device_name()
                    );
                    devices.push(super::AxDeviceEnum::Block(Box::new(raw)));
                }
            }
        }

        devices
    }
    #[cfg(not(target_os = "none"))]
    Vec::new()
}
