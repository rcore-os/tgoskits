use core::{
    any::Any,
    mem::offset_of,
    sync::atomic::{AtomicU32, Ordering},
};

use ax_fs_ng::vfs::FileBackend;
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use linux_raw_sys::{
    general::{O_ACCMODE, O_RDONLY},
    ioctl::{
        BLKFLSBUF, BLKGETSIZE, BLKGETSIZE64, BLKIOMIN, BLKIOOPT, BLKPG, BLKRAGET, BLKRASET,
        BLKROGET, BLKROSET, BLKRRPART, BLKSSZGET,
    },
    loop_device::{
        LO_FLAGS_AUTOCLEAR, LO_FLAGS_READ_ONLY, LOOP_CLR_FD, LOOP_CONFIGURE, LOOP_GET_STATUS,
        LOOP_GET_STATUS64, LOOP_SET_FD, LOOP_SET_STATUS, LOOP_SET_STATUS64, loop_config, loop_info,
        loop_info64,
    },
};

use crate::{
    file::{FileLike, get_file_like},
    mm::{UserPtr, VmMutPtr, VmPtr},
    pseudofs::{DeviceMmap, DeviceOps},
    sync::Mutex,
};

fn vm_error_to_vfs(error: starry_vm::VmError) -> VfsError {
    crate::StarryError::from(error).into()
}

/// HDIO_GETGEO ioctl command (get drive geometry).
/// Not defined in linux-raw-sys, so we use the standard value directly.
const HDIO_GETGEO: u32 = 0x0301;

/// /dev/loopX devices
pub struct LoopDevice {
    number: u32,
    dev_id: DeviceId,
    // Binding publication and open/close transitions share one lock.
    state: Mutex<LoopState>,
    ra: AtomicU32,
}

struct LoopBinding {
    file: FileBackend,
    file_name: [u8; 64],
    flags: u32,
    rundown: bool,
}

#[derive(Default)]
struct LoopState {
    binding: Option<LoopBinding>,
    openers: usize,
    exclusive: bool,
}

impl LoopState {
    fn binding(&self) -> VfsResult<&LoopBinding> {
        self.binding
            .as_ref()
            .filter(|binding| !binding.rundown)
            .ok_or(VfsError::NoSuchDeviceOrAddress)
    }

    fn binding_mut(&mut self) -> VfsResult<&mut LoopBinding> {
        self.binding
            .as_mut()
            .filter(|binding| !binding.rundown)
            .ok_or(VfsError::NoSuchDeviceOrAddress)
    }
}

impl LoopDevice {
    pub(crate) fn new(number: u32, dev_id: DeviceId) -> Self {
        Self {
            number,
            dev_id,
            state: Mutex::new(LoopState::default()),
            ra: AtomicU32::new(512),
        }
    }

    pub(crate) fn is_read_only(&self) -> VfsResult<bool> {
        Ok(self.state.lock().binding()?.flags & LO_FLAGS_READ_ONLY as u32 != 0)
    }

    fn bind(&self, file: &crate::file::File, name: [u8; 64], mut flags: u32) -> VfsResult<()> {
        let backend = file.inner().backend()?.clone();
        if file.open_flags() & O_ACCMODE == O_RDONLY {
            flags |= LO_FLAGS_READ_ONLY as u32;
        }
        let mut state = self.state.lock();
        if state.binding.is_some() {
            return Err(VfsError::ResourceBusy);
        }
        state.binding = Some(LoopBinding {
            file: backend,
            file_name: name,
            flags,
            rundown: false,
        });
        Ok(())
    }

    /// Get information about the loop device.
    pub fn get_info(&self) -> VfsResult<loop_info> {
        let state = self.state.lock();
        let binding = state.binding()?;
        let mut res: loop_info = unsafe { core::mem::zeroed() };
        res.lo_number = self.number as _;
        res.lo_rdevice = self.dev_id.0 as _;
        res.lo_flags = binding.flags as _;
        let name = &binding.file_name;
        for (i, &c) in name.iter().enumerate() {
            if i < 64 {
                res.lo_name[i] = c as _;
            }
            if c == 0 {
                break;
            }
        }
        Ok(res)
    }

    /// Get information about the loop device (64-bit variant).
    pub fn get_info64(&self) -> VfsResult<loop_info64> {
        let state = self.state.lock();
        let binding = state.binding()?;
        let mut res: loop_info64 = unsafe { core::mem::zeroed() };
        res.lo_number = self.number as _;
        res.lo_rdevice = self.dev_id.0 as _;
        res.lo_file_name = binding.file_name;
        res.lo_flags = binding.flags;
        Ok(res)
    }

    /// Clone the underlying file of the loop device.
    pub fn clone_file(&self) -> VfsResult<FileBackend> {
        Ok(self.state.lock().binding()?.file.clone())
    }
}

impl DeviceOps for LoopDevice {
    fn len(&self) -> VfsResult<u64> {
        self.clone_file()?.len()
    }

    fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.clone_file()?.sync(data_only)
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        self.clone_file()?.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let file = {
            let state = self.state.lock();
            let binding = state.binding()?;
            if binding.flags & LO_FLAGS_READ_ONLY as u32 != 0 {
                return Err(VfsError::ReadOnlyFilesystem);
            }
            binding.file.clone()
        };
        file.write_at(buf, offset)
    }

    fn open(&self, exclusive: bool) -> VfsResult<()> {
        let mut state = self.state.lock();
        if state
            .binding
            .as_ref()
            .is_some_and(|binding| binding.rundown)
        {
            return Err(VfsError::NoSuchDeviceOrAddress);
        }
        if state.exclusive {
            return Err(VfsError::ResourceBusy);
        }
        state.openers = state.openers.checked_add(1).ok_or(VfsError::ResourceBusy)?;
        state.exclusive |= exclusive;
        Ok(())
    }

    fn close(&self, exclusive: bool) {
        let released = {
            let mut state = self.state.lock();
            // Each successful device open owns exactly one final close.
            assert!(state.openers > 0);
            state.openers -= 1;
            if exclusive {
                state.exclusive = false;
            }
            if state.openers == 0
                && state
                    .binding
                    .as_ref()
                    .is_some_and(|binding| binding.flags & LO_FLAGS_AUTOCLEAR as u32 != 0)
            {
                state.binding.take()
            } else {
                None
            }
        };
        // Backing file destruction may enter another filesystem.
        drop(released);
    }

    fn ioctl(&self, current: &crate::task::UserTaskRef, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            LOOP_SET_FD => {
                let fd = arg as i32;
                if fd < 0 {
                    return Err(VfsError::BadFileDescriptor);
                }
                let f = get_file_like(fd)?;
                let Some(file) = f.downcast_ref::<crate::file::File>() else {
                    return Err(VfsError::InvalidInput);
                };
                self.bind(file, [0; 64], 0)?;
            }
            LOOP_CLR_FD => {
                let mut state = self.state.lock();
                let only_opener = state.openers == 1;
                let binding = state.binding_mut()?;
                binding.flags |= LO_FLAGS_AUTOCLEAR as u32;
                binding.rundown = only_opener;
            }
            LOOP_GET_STATUS => {
                write_loop_info(current, arg as *mut loop_info, self.get_info()?)?;
            }
            LOOP_SET_STATUS => {
                // `loop_info` is a C ioctl payload copied from the guest ABI.
                let info = unsafe {
                    (arg as *const loop_info)
                        .vm_read_uninit(current)
                        .map_err(vm_error_to_vfs)?
                        .assume_init()
                };
                let mut state = self.state.lock();
                let binding = state.binding_mut()?;
                let name = &mut binding.file_name;
                for (i, &c) in info.lo_name.iter().enumerate() {
                    if i < 64 {
                        name[i] = c as _;
                    }
                    if c == 0 {
                        break;
                    }
                }
                binding.flags = info.lo_flags as u32;
            }
            LOOP_GET_STATUS64 => {
                write_loop_info64(current, arg as *mut loop_info64, self.get_info64()?)?;
            }
            LOOP_SET_STATUS64 => {
                // `loop_info64` is a C ioctl payload copied from the guest ABI.
                let info = unsafe {
                    (arg as *const loop_info64)
                        .vm_read_uninit(current)
                        .map_err(vm_error_to_vfs)?
                        .assume_init()
                };
                let mut state = self.state.lock();
                let binding = state.binding_mut()?;
                binding.file_name = info.lo_file_name;
                binding.flags = info.lo_flags;
            }
            LOOP_CONFIGURE => {
                // `loop_config` is a C ioctl payload copied from the guest ABI.
                let cfg = unsafe {
                    (arg as *const loop_config)
                        .vm_read_uninit(current)
                        .map_err(vm_error_to_vfs)?
                        .assume_init()
                };
                let fd = cfg.fd as i32;
                if fd < 0 {
                    return Err(VfsError::BadFileDescriptor);
                }
                let f = get_file_like(fd)?;
                let Some(file) = f.downcast_ref::<crate::file::File>() else {
                    return Err(VfsError::InvalidInput);
                };
                self.bind(file, cfg.info.lo_file_name, cfg.info.lo_flags)?;
            }
            BLKGETSIZE | BLKGETSIZE64 => {
                let sectors = if let Ok(f) = self.clone_file() {
                    f.len()? / 512
                } else {
                    return Err(VfsError::NoSuchDeviceOrAddress);
                };
                if cmd == BLKGETSIZE {
                    (arg as *mut u32)
                        .vm_write(current, sectors as _)
                        .map_err(vm_error_to_vfs)?;
                } else {
                    (arg as *mut u64)
                        .vm_write(current, sectors * 512)
                        .map_err(vm_error_to_vfs)?;
                }
            }
            BLKSSZGET => {
                (arg as *mut u32)
                    .vm_write(current, 512)
                    .map_err(vm_error_to_vfs)?;
            }
            #[cfg(any(
                target_arch = "riscv64",
                target_arch = "aarch64",
                target_arch = "loongarch64"
            ))]
            linux_raw_sys::ioctl::BLKPBSZGET => {
                (arg as *mut u32)
                    .vm_write(current, 512)
                    .map_err(vm_error_to_vfs)?;
            }
            BLKROGET => {
                (arg as *mut u32)
                    .vm_write(current, self.is_read_only()? as u32)
                    .map_err(vm_error_to_vfs)?;
            }
            BLKROSET => {
                let ro = (arg as *const u32)
                    .vm_read(current)
                    .map_err(vm_error_to_vfs)?;
                if ro != 0 && ro != 1 {
                    return Err(VfsError::InvalidInput);
                }
                let mut state = self.state.lock();
                let binding = state.binding_mut()?;
                let mut flags = binding.flags;
                if ro != 0 {
                    flags |= LO_FLAGS_READ_ONLY as u32;
                } else {
                    flags &= !(LO_FLAGS_READ_ONLY as u32);
                }
                binding.flags = flags;
            }
            BLKRAGET => {
                (arg as *mut u32)
                    .vm_write(current, self.ra.load(Ordering::Relaxed))
                    .map_err(vm_error_to_vfs)?;
            }
            BLKRASET => {
                self.ra.store(
                    (arg as *const u32)
                        .vm_read(current)
                        .map_err(vm_error_to_vfs)? as _,
                    Ordering::Relaxed,
                );
            }
            BLKRRPART => {
                // loop device has no physical partition table; no-op
            }
            BLKPG => {
                // partition manipulation not supported on loop devices
                return Err(VfsError::NotATty);
            }
            BLKFLSBUF => {
                self.clone_file()?.sync(true)?;
            }
            BLKIOMIN => {
                // minimum I/O size
                (arg as *mut u32)
                    .vm_write(current, 512)
                    .map_err(vm_error_to_vfs)?;
            }
            BLKIOOPT => {
                // optimal I/O size
                (arg as *mut u32)
                    .vm_write(current, 512)
                    .map_err(vm_error_to_vfs)?;
            }
            // HDIO_GETGEO: virtual CHS geometry for fdisk
            HDIO_GETGEO => {
                let size = self.len()?;
                // hd_geometry: { u8 heads, u8 sectors, u16 cylinders, unsigned long start }
                // On 64-bit targets unsigned long is 8 bytes.
                let heads: u8 = 64;
                let sectors: u8 = 32;
                let cyl = if size > 0 {
                    (size / (heads as u64 * sectors as u64 * 512)) as u16
                } else {
                    0
                };
                #[repr(C)]
                #[derive(Clone, Copy, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
                struct HdGeometry {
                    heads: u8,
                    sectors: u8,
                    cylinders: u16,
                    _padding: u32,
                    start: u64,
                }
                let geo = HdGeometry {
                    heads,
                    sectors,
                    cylinders: cyl,
                    _padding: 0,
                    start: 0,
                };
                (arg as *mut HdGeometry)
                    .vm_write(current, geo)
                    .map_err(vm_error_to_vfs)?;
            }
            _ => {
                warn!("unknown ioctl for loop device: {cmd}");
                return Err(VfsError::NotATty);
            }
        }
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn mmap(&self, _offset: u64, _length: u64) -> DeviceMmap {
        if let Ok(FileBackend::Cached(cache)) = self.clone_file() {
            DeviceMmap::Cache(cache)
        } else {
            DeviceMmap::None
        }
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

fn write_loop_info(
    current: &crate::task::UserTaskRef,
    user: *mut loop_info,
    info: loop_info,
) -> VfsResult<()> {
    let user = UserPtr::from(user);
    user.write_field(current, offset_of!(loop_info, lo_number), info.lo_number)?;
    user.write_field(current, offset_of!(loop_info, lo_device), info.lo_device)?;
    user.write_field(current, offset_of!(loop_info, lo_inode), info.lo_inode)?;
    user.write_field(current, offset_of!(loop_info, lo_rdevice), info.lo_rdevice)?;
    user.write_field(current, offset_of!(loop_info, lo_offset), info.lo_offset)?;
    user.write_field(
        current,
        offset_of!(loop_info, lo_encrypt_type),
        info.lo_encrypt_type,
    )?;
    user.write_field(
        current,
        offset_of!(loop_info, lo_encrypt_key_size),
        info.lo_encrypt_key_size,
    )?;
    user.write_field(current, offset_of!(loop_info, lo_flags), info.lo_flags)?;
    user.write_field(current, offset_of!(loop_info, lo_name), info.lo_name)?;
    user.write_field(
        current,
        offset_of!(loop_info, lo_encrypt_key),
        info.lo_encrypt_key,
    )?;
    user.write_field(current, offset_of!(loop_info, lo_init), info.lo_init)?;
    Ok(user.write_field(current, offset_of!(loop_info, reserved), info.reserved)?)
}

fn write_loop_info64(
    current: &crate::task::UserTaskRef,
    user: *mut loop_info64,
    info: loop_info64,
) -> VfsResult<()> {
    let user = UserPtr::from(user);
    user.write_field(current, offset_of!(loop_info64, lo_device), info.lo_device)?;
    user.write_field(current, offset_of!(loop_info64, lo_inode), info.lo_inode)?;
    user.write_field(
        current,
        offset_of!(loop_info64, lo_rdevice),
        info.lo_rdevice,
    )?;
    user.write_field(current, offset_of!(loop_info64, lo_offset), info.lo_offset)?;
    user.write_field(
        current,
        offset_of!(loop_info64, lo_sizelimit),
        info.lo_sizelimit,
    )?;
    user.write_field(current, offset_of!(loop_info64, lo_number), info.lo_number)?;
    user.write_field(
        current,
        offset_of!(loop_info64, lo_encrypt_type),
        info.lo_encrypt_type,
    )?;
    user.write_field(
        current,
        offset_of!(loop_info64, lo_encrypt_key_size),
        info.lo_encrypt_key_size,
    )?;
    user.write_field(current, offset_of!(loop_info64, lo_flags), info.lo_flags)?;
    user.write_field(
        current,
        offset_of!(loop_info64, lo_file_name),
        info.lo_file_name,
    )?;
    user.write_field(
        current,
        offset_of!(loop_info64, lo_crypt_name),
        info.lo_crypt_name,
    )?;
    user.write_field(
        current,
        offset_of!(loop_info64, lo_encrypt_key),
        info.lo_encrypt_key,
    )?;
    Ok(user.write_field(current, offset_of!(loop_info64, lo_init), info.lo_init)?)
}
