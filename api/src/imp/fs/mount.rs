use core::ffi::{c_char, c_void};

use alloc::sync::Arc;
use axdriver::prelude::{BaseDriverOps, BlockDriverOps, DevError, DevResult, DeviceType};
use axerrno::LinuxResult;
use axfs_ng::{FS_CONTEXT, File};
use axfs_ng_vfs::NodeType;
use axio::Write;
use axsync::{Mutex, RawMutex};
use starry_core::vfs::{Device, MemoryFs};

use crate::ptr::UserConstPtr;

#[allow(unused)]
const BLOCK_SIZE: u64 = 512;

#[allow(unused)]
struct FileBlockDevice(Arc<Mutex<File<RawMutex>>>);
impl BaseDriverOps for FileBlockDevice {
    fn device_name(&self) -> &str {
        "loop"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }
}
impl BlockDriverOps for FileBlockDevice {
    fn num_blocks(&self) -> u64 {
        self.0.lock().inner().len().unwrap() / BLOCK_SIZE
    }
    fn block_size(&self) -> usize {
        BLOCK_SIZE as usize
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> DevResult {
        self.0
            .lock()
            .read_at(buf, block_id * BLOCK_SIZE)
            .map(|_| ())
            .map_err(|_| DevError::Io)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> DevResult {
        self.0
            .lock()
            .write_at(buf, block_id * BLOCK_SIZE)
            .map(|_| ())
            .map_err(|_| DevError::Io)
    }

    fn flush(&mut self) -> DevResult {
        self.0.lock().flush().map_err(|_| DevError::Io)
    }
}

pub fn sys_mount(
    source: UserConstPtr<c_char>,
    target: UserConstPtr<c_char>,
    fs_type: UserConstPtr<c_char>,
    _flags: i32,
    _data: UserConstPtr<c_void>,
) -> LinuxResult<isize> {
    let source = source.get_as_str()?;
    let target = target.get_as_str()?;
    let fs_type = fs_type.get_as_str()?;
    info!(
        "sys_mount <= source: {:?}, target: {:?}, fs_type: {:?}",
        source, target, fs_type
    );

    /* let fs_maker: fn(AxBlockDevice) -> LinuxResult<axfs_ng_vfs::Filesystem<RawMutex>> =
    match fs_type {
        // "fat" => |dev| Ok(axfs_ng::fs::fat::FatFilesystem::new(dev)),
        // "ext4" => axfs_ng::fs::ext4::Ext4Filesystem::new,
        "tmpfs" => |_| Ok(MemoryFs::new()),
        _ => return Err(axerrno::LinuxError::ENODEV),
    }; */
    if fs_type != "tmpfs" {
        return Err(axerrno::LinuxError::ENODEV);
    }

    let source = FS_CONTEXT.lock().resolve(source)?;
    if source.metadata()?.node_type != NodeType::BlockDevice {
        return Err(axerrno::LinuxError::ENOTBLK);
    }
    let _device = source
        .entry()
        .as_file()?
        .inner()
        .clone()
        .into_any()
        .downcast::<Device<RawMutex>>()
        .map_err(|_| axerrno::LinuxError::ENOTBLK)?;
    /* // TODO: only loop device is supported
    let ops = device.inner().as_any();
    let Some(loop_device) = ops.downcast_ref::<LoopDevice>() else {
        return Err(axerrno::LinuxError::ENOTBLK);
    };
    let file = loop_device.clone_file()?; */

    let fs = MemoryFs::new();

    let target = FS_CONTEXT.lock().resolve(target)?;
    target.mount(&fs)?;

    Ok(0)
}

pub fn sys_umount2(target: UserConstPtr<c_char>, _flags: i32) -> LinuxResult<isize> {
    let target = target.get_as_str()?;
    info!("sys_umount2 <= target: {:?}", target);
    let target = FS_CONTEXT.lock().resolve(target)?;
    target.unmount()?;
    Ok(0)
}
