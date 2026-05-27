use core::{
    any::Any,
    mem::{MaybeUninit, size_of},
};

use ax_memory_addr::{PhysAddr, PhysAddrRange};
use ax_runtime::hal::{cpu::asm::user_copy, mem::phys_to_virt};
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use k230_kpu::{
    CommandRange, KPU_CFG_PADDR, KPU_CFG_SIZE, KPU_IOC_CLEAR, KPU_IOC_GET_STATUS,
    KPU_IOC_PROGRAM_COMMAND, KPU_IOC_RUN, KPU_IOC_START, KPU_IOC_WAIT_DONE, KPU_L2_PADDR,
    KPU_L2_SIZE, KPU_MMAP_CFG_OFFSET, KPU_MMAP_L2_OFFSET, Kpu,
};

use crate::pseudofs::{DeviceMmap, DeviceOps};

pub const KPU_DEVICE_ID: DeviceId = DeviceId::new(240, 1);
const KPU_CFG_MMAP_PADDR_OFFSET: u64 = KPU_CFG_PADDR as u64;
const KPU_L2_MMAP_PADDR_OFFSET: u64 = KPU_L2_PADDR as u64;

pub struct KpuDevice {
    hw: Kpu,
}

impl KpuDevice {
    pub fn new() -> Self {
        let base_vaddr = phys_to_virt(PhysAddr::from(KPU_CFG_PADDR)).as_usize();
        Self {
            hw: unsafe { Kpu::new(base_vaddr) },
        }
    }

    fn copy_command_range(arg: usize) -> VfsResult<CommandRange> {
        copy_from_user(arg)
    }
}

impl Default for KpuDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceOps for KpuDevice {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        if buf.len() < size_of::<u32>() || !offset.is_multiple_of(size_of::<u32>() as u64) {
            return Err(VfsError::InvalidInput);
        }
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        if offset + size_of::<u32>() > KPU_CFG_SIZE {
            return Err(VfsError::InvalidInput);
        }
        let value = self.hw.read_reg(offset).to_ne_bytes();
        let len = buf.len().min(value.len());
        buf[..len].copy_from_slice(&value[..len]);
        Ok(len)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if buf.len() < size_of::<u32>() || !offset.is_multiple_of(size_of::<u32>() as u64) {
            return Err(VfsError::InvalidInput);
        }
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        if offset + size_of::<u32>() > KPU_CFG_SIZE {
            return Err(VfsError::InvalidInput);
        }
        let mut bytes = [0_u8; size_of::<u32>()];
        bytes.copy_from_slice(&buf[..size_of::<u32>()]);
        self.hw.write_reg(offset, u32::from_ne_bytes(bytes));
        Ok(size_of::<u32>())
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            KPU_IOC_GET_STATUS => {
                let status = self.hw.status();
                copy_to_user(arg, &status)?;
                Ok(0)
            }
            KPU_IOC_CLEAR => {
                self.hw.clear_done();
                Ok(0)
            }
            KPU_IOC_PROGRAM_COMMAND => {
                let range = Self::copy_command_range(arg)?;
                self.hw
                    .program_command(range)
                    .map_err(|_| VfsError::InvalidInput)?;
                Ok(0)
            }
            KPU_IOC_START => {
                self.hw.start();
                Ok(0)
            }
            KPU_IOC_RUN => {
                let range = Self::copy_command_range(arg)?;
                self.hw
                    .run_command(range)
                    .map_err(|_| VfsError::InvalidInput)?;
                Ok(0)
            }
            KPU_IOC_WAIT_DONE => {
                let poll_limit = if arg == 0 { 1_000_000 } else { arg };
                self.hw
                    .wait_done(poll_limit)
                    .map_err(|_| VfsError::TimedOut)?;
                Ok(0)
            }
            _ => Err(VfsError::OperationNotSupported),
        }
    }

    fn mmap(&self, offset: u64, length: u64) -> DeviceMmap {
        let Some(length) = usize::try_from(length).ok() else {
            return DeviceMmap::None;
        };
        match offset {
            KPU_MMAP_CFG_OFFSET | KPU_CFG_MMAP_PADDR_OFFSET => {
                DeviceMmap::Physical(PhysAddrRange::from_start_size(
                    PhysAddr::from(KPU_CFG_PADDR),
                    length.min(KPU_CFG_SIZE),
                ))
            }
            KPU_MMAP_L2_OFFSET | KPU_L2_MMAP_PADDR_OFFSET => {
                DeviceMmap::Physical(PhysAddrRange::from_start_size(
                    PhysAddr::from(KPU_L2_PADDR),
                    length.min(KPU_L2_SIZE),
                ))
            }
            _ => DeviceMmap::None,
        }
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn copy_from_user<T: Copy>(arg: usize) -> VfsResult<T> {
    if arg == 0 {
        return Err(VfsError::InvalidInput);
    }
    let mut value = MaybeUninit::<T>::uninit();
    let ret = unsafe {
        user_copy(
            value.as_mut_ptr().cast::<u8>(),
            arg as *const u8,
            size_of::<T>(),
        )
    };
    if ret != 0 {
        return Err(VfsError::InvalidData);
    }
    Ok(unsafe { value.assume_init() })
}

fn copy_to_user<T: Copy>(arg: usize, value: &T) -> VfsResult<()> {
    if arg == 0 {
        return Err(VfsError::InvalidInput);
    }
    let ret = unsafe {
        user_copy(
            arg as *mut u8,
            (value as *const T).cast::<u8>(),
            size_of::<T>(),
        )
    };
    if ret != 0 {
        return Err(VfsError::InvalidData);
    }
    Ok(())
}
