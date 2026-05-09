use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs::FileBackend;
use ax_sync::Mutex;
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsResult};
use linux_raw_sys::{
    ioctl::{
        BLKDISCARD, BLKGETSIZE, BLKGETSIZE64, BLKRAGET, BLKRASET, BLKROGET, BLKROSET, BLKSSZGET,
    },
    loop_device::{LOOP_CLR_FD, LOOP_GET_STATUS, LOOP_SET_FD, LOOP_SET_STATUS, loop_info},
};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    file::get_file_like,
    pseudofs::{DeviceMmap, DeviceOps},
};

/// /dev/loopX devices
pub struct LoopDevice {
    number: u32,
    dev_id: DeviceId,
    /// Underlying file for the loop device, if any.
    pub file: Mutex<Option<FileBackend>>,
    /// Read-only flag for the loop device.
    pub ro: AtomicBool,
    /// Read-ahead size for the loop device, in bytes.
    pub ra: AtomicU32,
    /// True while an O_EXCL opener holds the device. Non-exclusive opens
    /// and further exclusive opens are rejected with EBUSY while set.
    exclusive: AtomicBool,
}

impl LoopDevice {
    pub(crate) fn new(number: u32, dev_id: DeviceId) -> Self {
        Self {
            number,
            dev_id,
            file: Mutex::new(None),
            ro: AtomicBool::new(false),
            ra: AtomicU32::new(512),
            exclusive: AtomicBool::new(false),
        }
    }

    fn is_bound(&self) -> bool {
        self.file.lock().is_some()
    }

    /// Get information about the loop device.
    pub fn get_info(&self) -> AxResult<loop_info> {
        if self.file.lock().is_none() {
            return Err(AxError::from(LinuxError::ENXIO));
        }
        let mut res: loop_info = unsafe { core::mem::zeroed() };
        res.lo_number = self.number as _;
        res.lo_rdevice = self.dev_id.0 as _;
        Ok(res)
    }

    /// Set information for the loop device.
    pub fn set_info(&self, _src: loop_info) -> AxResult<()> {
        Ok(())
    }

    /// Clone the underlying file of the loop device.
    pub fn clone_file(&self) -> VfsResult<FileBackend> {
        let file = self.file.lock().clone();
        file.ok_or(AxError::from(LinuxError::ENXIO))
    }
}

impl DeviceOps for LoopDevice {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let file = self.file.lock().clone();
        file.ok_or(AxError::OperationNotPermitted)?
            .read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if self.ro.load(Ordering::Relaxed) {
            return Err(AxError::ReadOnlyFilesystem);
        }
        let file = self.file.lock().clone();
        file.ok_or(AxError::OperationNotPermitted)?
            .write_at(buf, offset)
    }

    fn open(&self, exclusive: bool) -> VfsResult<()> {
        if exclusive {
            if self.exclusive.swap(true, Ordering::Acquire) {
                return Err(AxError::ResourceBusy);
            }
        } else if self.exclusive.load(Ordering::Acquire) {
            return Err(AxError::ResourceBusy);
        }
        Ok(())
    }

    fn close(&self, exclusive: bool) {
        if exclusive {
            self.exclusive.store(false, Ordering::Release);
        }
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            LOOP_SET_FD => {
                let fd = arg as i32;
                if fd < 0 {
                    return Err(AxError::BadFileDescriptor);
                }
                let f = get_file_like(fd)?;
                let Some(file) = f.downcast_ref::<crate::file::File>() else {
                    return Err(AxError::InvalidInput);
                };
                let mut guard = self.file.lock();
                if guard.is_some() {
                    return Err(AxError::ResourceBusy);
                }

                *guard = Some(file.inner().backend()?.clone());
            }
            LOOP_CLR_FD => {
                let mut guard = self.file.lock();
                if guard.is_none() {
                    return Err(AxError::from(LinuxError::ENXIO));
                }
                *guard = None;
            }
            LOOP_GET_STATUS => {
                (arg as *mut loop_info).vm_write(self.get_info()?)?;
            }
            LOOP_SET_STATUS => {
                // FIXME: AnyBitPattern
                let info = unsafe { (arg as *const loop_info).vm_read_uninit()?.assume_init() };
                self.set_info(info)?;
            }
            BLKSSZGET => {
                (arg as *mut u32).vm_write(512)?;
            }
            BLKDISCARD => {
                if !self.is_bound() {
                    return Err(AxError::from(LinuxError::ENXIO));
                }
                if self.ro.load(Ordering::Relaxed) {
                    return Err(AxError::ReadOnlyFilesystem);
                }
                let ptr = arg as *const u64;
                let start = ptr.vm_read()?;
                let len = unsafe { ptr.add(1) }.vm_read()?;
                if len == 0 {
                    return Err(AxError::InvalidInput);
                }
                let dev_size = self.clone_file()?.location().len()?;
                if start.saturating_add(len) > dev_size {
                    return Err(AxError::InvalidInput);
                }
                // Real discard (punch-hole on backing file) not yet implemented.
                return Err(AxError::OperationNotSupported);
            }
            BLKGETSIZE | BLKGETSIZE64 => {
                let sectors = if let Ok(f) = self.clone_file() {
                    f.location().len()? / 512
                } else {
                    return Err(AxError::from(LinuxError::ENXIO));
                };
                if cmd == BLKGETSIZE {
                    (arg as *mut u32).vm_write(sectors as _)?;
                } else {
                    (arg as *mut u64).vm_write(sectors * 512)?;
                }
            }
            BLKROGET => {
                (arg as *mut u32).vm_write(self.ro.load(Ordering::Relaxed) as u32)?;
            }
            BLKROSET => {
                let ro = (arg as *const u32).vm_read()?;
                if ro != 0 && ro != 1 {
                    return Err(AxError::InvalidInput);
                }
                self.ro.store(ro != 0, Ordering::Relaxed);
            }
            BLKRAGET => {
                (arg as *mut u32).vm_write(self.ra.load(Ordering::Relaxed))?;
            }
            BLKRASET => {
                self.ra
                    .store((arg as *const u32).vm_read()? as _, Ordering::Relaxed);
            }
            _ => {
                warn!("unknown ioctl for loop device: {cmd}");
                return Err(AxError::NotATty);
            }
        }
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn mmap(&self, _offset: u64) -> DeviceMmap {
        if let Some(FileBackend::Cached(cache)) = self.file.lock().as_ref() {
            DeviceMmap::Cache(cache.clone())
        } else {
            DeviceMmap::None
        }
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}
