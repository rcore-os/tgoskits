#[cfg(feature = "block")]
use alloc::boxed::Box;
use alloc::vec::Vec;

#[cfg(feature = "block")]
use ax_driver_base::{BaseDriverOps, DevError, DeviceType};
#[cfg(feature = "block")]
use ax_driver_block::{
    BlockDriverOps,
    gpt::{GptPartitionDev, find_partition_range, is_gpt_disk},
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
                            warn!(
                                "GPT detected on block device {}, but no partition named 'rootfs' \
                                 was found; using raw device",
                                raw.device_name()
                            );
                            devices.push(super::AxDeviceEnum::Block(Box::new(raw)));
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
