use ax_driver::AxBlockDevice;
use ax_driver_block::partition::PartitionRegion;
use axfs_ng_vfs::{Filesystem, VfsResult};

cfg_if::cfg_if! {
    if #[cfg(feature = "ext4")] {
        mod ext4;
        type DefaultFilesystem = ext4::Ext4Filesystem;
    } else if #[cfg(feature = "fat")] {
        mod fat;
        type DefaultFilesystem = fat::FatFilesystem;
    } else {
        struct DefaultFilesystem;
        impl DefaultFilesystem {
            pub fn new_in_region(_dev: AxBlockDevice, _region: Option<PartitionRegion>) -> VfsResult<Filesystem> {
                panic!("No filesystem feature enabled");
            }
        }
    }
}

pub fn new_default_in_region(
    dev: AxBlockDevice,
    region: Option<PartitionRegion>,
) -> VfsResult<Filesystem> {
    DefaultFilesystem::new_in_region(dev, region)
}
