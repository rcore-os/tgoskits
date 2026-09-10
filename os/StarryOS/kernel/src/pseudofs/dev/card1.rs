use alloc::{borrow::Cow, collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
use core::{
    any::Any,
    convert::TryFrom,
    ffi::CStr,
    mem,
};

use ax_driver::rknpu::{
    self, GemCachePolicy, RknpuAction, RknpuMemCreate, RknpuMemDestroy, RknpuMemMap, RknpuMemSync,
    RknpuSubmit, RknpuTask,
};
use ax_memory_addr::{PhysAddr, PhysAddrRange};
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, Pollable};
use bytemuck::{AnyBitPattern, NoUninit};
use linux_raw_sys::general::O_CLOEXEC;

use super::drm::{DrmUnique, DrmVersion};
use crate::{
    StarryError, StarryResult,
    file::{
        File as KernelFile, FileLike, IoDst, IoSrc, Kstat,
        dmabuf::{ContiguousDmaBuf, resolve_contiguous_dmabuf},
    },
    mm::{vm_load, vm_write_slice},
    pseudofs::{
        DeviceOps,
        dev::drm::{io_size, ioctl_nr, is_driver_ioctl},
        device::DeviceMmap,
    },
    task::UserTaskRef,
    sync::Mutex,
};

/// Driver name for DRM device
const DRM1_NAME: &CStr = c"rknpu";
/// Driver date for DRM device
const DRM1_DATE: &CStr = c"20240828";
/// Driver description for DRM device
const DRM1_DESC: &CStr = c"RKNPU driver";

/// Device ID for /dev/dri/card1
pub const CARD1_SYSTEM_DEVICE_ID: DeviceId = DeviceId::new(0xe2, 1);

/// Page shift constant (4KB pages)
const PAGE_SHIFT: u32 = 12;
/// Maximum ioctl command number
const MAX_IOCTL_NR: u32 = 0xcf;
/// Stack data buffer size
const STACK_DATA_SIZE: usize = 128;

/// Storage for DRM ioctl arguments whose ABI contains 64-bit fields or pointers.
#[repr(align(8))]
struct AlignedIoctlData([u8; STACK_DATA_SIZE]);

/// DRM ioctl version command number
const DRM_IOCTL_VERSION_NR: u32 = 0;
/// DRM ioctl get unique command number
const DRM_IOCTL_GET_UNIQUE_NR: u32 = 1;
/// DRM ioctl gem flink command number
const DRM_IOCTL_GEM_FLINK_NR: u32 = 10;
/// DRM ioctl prime handle to fd command number
const DRM_IOCTL_PRIME_HANDLE_TO_FD_NR: u32 = 0x2d;
/// DRM ioctl prime fd to handle command number (import an external dma-buf)
const DRM_IOCTL_PRIME_FD_TO_HANDLE_NR: u32 = 0x2e;

/// GEM handles whose destruction could not acquire the global NPU lock yet.
/// The backing allocations remain owned by the driver until a later card1
/// operation retries these handles.
static DEFERRED_GEM_DESTROYS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// RKNPU command types
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RknpuCmd {
    /// Action command
    Action     = 0x00,
    /// Submit command
    Submit     = 0x01,
    /// Memory create command
    MemCreate  = 0x02,
    /// Memory map command
    MemMap     = 0x03,
    /// Memory destroy command
    MemDestroy = 0x04,
    /// Memory sync command
    MemSync    = 0x05,
}

impl TryFrom<u32> for RknpuCmd {
    type Error = ();

    /// Tries to convert a u32 value to an RknpuCmd
    fn try_from(nr: u32) -> Result<Self, Self::Error> {
        match nr {
            0x00 | 0x40 => Ok(RknpuCmd::Action),
            0x01 | 0x41 => Ok(RknpuCmd::Submit),
            0x02 | 0x42 => Ok(RknpuCmd::MemCreate),
            0x03 | 0x43 => Ok(RknpuCmd::MemMap),
            0x04 | 0x44 => Ok(RknpuCmd::MemDestroy),
            0x05 | 0x45 => Ok(RknpuCmd::MemSync),
            _ => {
                warn!("Unknown ioctl nr: {nr:#x}",);
                Err(())
            }
        }
    }
}

/// Represents an RKNPU user action with flags and value
#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern, NoUninit)]
struct RknpuUserAction {
    /// Action flags
    pub flags: u32,
    /// Action value
    pub value: u32,
}

fn decode_rknpu_action(raw: u32) -> VfsResult<RknpuAction> {
    let action = match raw {
        0 => RknpuAction::GetHwVersion,
        1 => RknpuAction::GetDrvVersion,
        2 => RknpuAction::GetFreq,
        3 => RknpuAction::SetFreq,
        4 => RknpuAction::GetVolt,
        5 => RknpuAction::SetVolt,
        6 => RknpuAction::ActReset,
        7 => RknpuAction::GetBwPriority,
        8 => RknpuAction::SetBwPriority,
        9 => RknpuAction::GetBwExpect,
        10 => RknpuAction::SetBwExpect,
        11 => RknpuAction::GetBwTw,
        12 => RknpuAction::SetBwTw,
        13 => RknpuAction::ActClrTotalRwAmount,
        14 => RknpuAction::GetDtWrAmount,
        15 => RknpuAction::GetDtRdAmount,
        16 => RknpuAction::GetWtRdAmount,
        17 => RknpuAction::GetTotalRwAmount,
        18 => RknpuAction::GetIommuEn,
        19 => RknpuAction::SetProcNice,
        20 => RknpuAction::PowerOn,
        21 => RknpuAction::PowerOff,
        22 => RknpuAction::GetTotalSramSize,
        23 => RknpuAction::GetFreeSramSize,
        24 => RknpuAction::GetIommuDomainId,
        25 => RknpuAction::SetIommuDomainId,
        _ => return Err(VfsError::InvalidInput),
    };
    Ok(action)
}

/// DRM card1 device implementation
pub struct Card1;

impl Card1 {
    /// Creates a new /dev/dri/card1 device.
    pub fn new() -> Card1 {
        Self
    }
}

impl Default for Card1 {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceOps for Card1 {
    /// Reads data from the device (not supported for card1)
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        trace!("dri: read_at called");
        // card1 heap devices are not meant to be read directly
        Err(VfsError::InvalidInput)
    }

    /// Writes data to the device (not supported for card1)
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        trace!("dri: write_at called");
        // card1 heap devices are not meant to be written directly
        Err(VfsError::InvalidInput)
    }

    /// Handles ioctl commands for the device
    fn ioctl(
        &self,
        _current: &UserTaskRef,
        _cmd: u32,
        _arg: usize,
    ) -> VfsResult<usize> {
        // A bare device node has no per-open GEM namespace. Opens are wrapped
        // by `Card1File` below, which supplies the Linux `file->private_data`
        // equivalent and owns all handles visible to that open.
        Err(VfsError::NotATty)
    }

    /// Returns a reference to the object as Any for dynamic type checking
    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Returns the node flags for the device
    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }

    /// The node itself is not mappable because it has no open-file handle
    /// namespace. `Card1File::device_mmap` performs the owned lookup.
    fn mmap(&self, _offset: u64, _length: u64) -> DeviceMmap {
        DeviceMmap::None
    }
}

/// True if `inner` is the `/dev/dri/card1` node.
pub(crate) fn is_card1_device(inner: &dyn Any) -> bool {
    inner.is::<Card1>()
}

/// Build the per-open card1 file. The returned `Arc` is shared by `dup` and
/// `fork`, so its GEM namespace has the same lifetime as the open file
/// description rather than a process id.
pub(crate) fn open_card1_file(
    file: ax_fs_ng::File,
    open_flags: u32,
) -> StarryResult<Arc<dyn FileLike>> {
    retry_deferred_gem_destroys();
    Ok(Arc::new(Card1File::new(KernelFile::new(file, open_flags))))
}

struct Card1File {
    base: KernelFile,
    /// Local handles are translated to global driver handles. This prevents a
    /// handle obtained from another card1 open from reaching the NPU GEM pool.
    handles: Mutex<BTreeMap<u32, u32>>,
    next_handle: Mutex<u32>,
    /// Serializes operations that validate and then use a GEM buffer. In
    /// particular, `MemDestroy` cannot free a buffer between submit validation
    /// and programming its DMA address.
    operation: Mutex<()>,
}

impl Card1File {
    fn new(base: KernelFile) -> Self {
        Self {
            base,
            handles: Mutex::new(BTreeMap::new()),
            next_handle: Mutex::new(1),
            operation: Mutex::new(()),
        }
    }

    fn add_handle(&self, global_handle: u32) -> VfsResult<u32> {
        let mut handles = self.handles.lock();
        let mut next = self.next_handle.lock();
        for _ in 0..=u32::MAX {
            let handle = *next;
            *next = handle.wrapping_add(1);
            if handle == 0 || handles.contains_key(&handle) {
                continue;
            }
            handles.insert(handle, global_handle);
            return Ok(handle);
        }
        Err(VfsError::NoMemory)
    }

    fn global_handle(&self, handle: u32) -> VfsResult<u32> {
        self.handles
            .lock()
            .get(&handle)
            .copied()
            .ok_or(VfsError::InvalidInput)
    }

    fn remove_handle(&self, handle: u32) -> VfsResult<u32> {
        self.handles
            .lock()
            .remove(&handle)
            .ok_or(VfsError::InvalidInput)
    }

    fn exported_gem_buffer(&self, handle: u32) -> StarryResult<ExportedGemBuffer> {
        let global_handle = self.global_handle(handle)?;
        exported_gem_buffer(global_handle)
    }

    fn find_dma_range(&self, address: u64, length: u64) -> bool {
        let handles: Vec<u32> = self.handles.lock().values().copied().collect();
        handles.into_iter().any(|global_handle| {
            let Ok(info) = rknpu::buffer_info(global_handle) else {
                return false;
            };
            range_contains(info.dma_addr, info.size, address, length)
        })
    }

    fn find_cpu_range(&self, address: u64, offset: u64, length: u64) -> bool {
        let handles: Vec<u32> = self.handles.lock().values().copied().collect();
        handles.into_iter().any(|global_handle| {
            let Ok((base, size)) = rknpu::obj_addr_and_size(global_handle) else {
                return false;
            };
            let Some(address_offset) = address.checked_sub(base as u64) else {
                return false;
            };
            if address_offset >= size as u64 {
                return false;
            }
            let Some(total_offset) = address_offset.checked_add(offset) else {
                return false;
            };
            range_contains(0, size, total_offset, length)
        })
    }

    fn submit_task_span(args: &RknpuSubmit) -> VfsResult<(usize, usize)> {
        const MAX_TASKS: usize = 4095;
        let mut first = usize::MAX;
        let mut end = 0usize;
        let mut add_range = |start: u32, number: u32| -> VfsResult<()> {
            if number == 0 {
                return Ok(());
            }
            let start = start as usize;
            let range_end = start.checked_add(number as usize).ok_or(VfsError::InvalidData)?;
            first = first.min(start);
            end = end.max(range_end);
            Ok(())
        };

        add_range(args.task_start, args.task_number)?;
        for subcore in args.subcore_task {
            add_range(subcore.task_start, subcore.task_number)?;
        }
        if first == usize::MAX || end.checked_sub(first).is_none_or(|len| len > MAX_TASKS) {
            return Err(VfsError::InvalidData);
        }
        Ok((first, end))
    }

    fn load_tasks(
        &self,
        current: &UserTaskRef,
        args: &RknpuSubmit,
    ) -> VfsResult<(usize, Vec<RknpuTask>)> {
        let (first, end) = Self::submit_task_span(args)?;
        let task_size = mem::size_of::<RknpuTask>();
        let byte_offset = first.checked_mul(task_size).ok_or(VfsError::BadAddress)?;
        let byte_len = (end - first)
            .checked_mul(task_size)
            .ok_or(VfsError::BadAddress)?;
        let task_addr = (args.task_obj_addr as usize)
            .checked_add(byte_offset)
            .ok_or(VfsError::BadAddress)?;
        if task_addr == 0 {
            return Err(VfsError::BadAddress);
        }

        let bytes: Vec<u8> = vm_load(current, task_addr as *const u8, byte_len)
            .map_err(|_| VfsError::BadAddress)?;
        let mut tasks = Vec::with_capacity(end - first);
        for bytes in bytes.chunks_exact(task_size) {
            // `RknpuTask` is `repr(C, packed)` and contains integer fields only.
            // The source is an initialized kernel-owned byte vector, so an
            // unaligned read is the exact operation needed to decode the ABI.
            let task = unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<RknpuTask>()) };
            tasks.push(task);
        }
        Ok((first, tasks))
    }

    fn validate_submit_dma(
        &self,
        args: &RknpuSubmit,
        tasks: &[RknpuTask],
        task_bytes: u64,
    ) -> VfsResult<()> {
        if args.task_base_addr > u32::MAX as u64
            || !self.find_dma_range(args.task_base_addr, task_bytes)
        {
            return Err(VfsError::InvalidData);
        }
        for task in tasks {
            let command_bytes = (task.regcfg_amount as u64)
                .checked_mul(mem::size_of::<u64>() as u64)
                .ok_or(VfsError::InvalidData)?;
            if command_bytes == 0
                || task.regcmd_addr > u32::MAX as u64
                || !self.find_dma_range(task.regcmd_addr, command_bytes)
            {
                return Err(VfsError::InvalidData);
            }
        }
        Ok(())
    }

    fn handle_submit(&self, current: &UserTaskRef, args: &mut RknpuSubmit) -> VfsResult<()> {
        let (first, mut tasks) = self.load_tasks(current, args)?;
        let task_bytes = (tasks.len() as u64)
            .checked_mul(mem::size_of::<RknpuTask>() as u64)
            .ok_or(VfsError::BadAddress)?;
        self.validate_submit_dma(args, &tasks, task_bytes)?;

        let mut driver_args = *args;
        if driver_args.task_number != 0 {
            driver_args.task_start = driver_args
                .task_start
                .checked_sub(first as u32)
                .ok_or(VfsError::InvalidData)?;
        }
        for subcore in &mut driver_args.subcore_task {
            if subcore.task_number != 0 {
                subcore.task_start = subcore
                    .task_start
                    .checked_sub(first as u32)
                    .ok_or(VfsError::InvalidData)?;
            }
        }
        rknpu::submit(&mut driver_args, &mut tasks).map_err(map_rknpu_err)?;
        args.task_counter = driver_args.task_counter;
        args.hw_elapse_time = driver_args.hw_elapse_time;

        let task_addr = (args.task_obj_addr as usize)
            .checked_add(first.checked_mul(mem::size_of::<RknpuTask>()).ok_or(VfsError::BadAddress)?)
            .ok_or(VfsError::BadAddress)?;
        // SAFETY: `tasks` is an initialized, contiguous vector of packed ABI
        // records, and `task_bytes` is exactly its byte length.
        let task_bytes = unsafe {
            core::slice::from_raw_parts(tasks.as_ptr().cast::<u8>(), task_bytes as usize)
        };
        vm_write_slice(current, task_addr as *mut u8, task_bytes)
            .map_err(|_| VfsError::BadAddress)?;
        Ok(())
    }
}

/// Return whether `[address, address + length)` is fully contained in a GEM
/// buffer. All arithmetic is checked because both values originate in ioctl
/// data and are later programmed into a device-visible address register.
fn range_contains(base: u64, size: usize, address: u64, length: u64) -> bool {
    let Some(buffer_end) = base.checked_add(size as u64) else {
        return false;
    };
    let Some(end) = address.checked_add(length) else {
        return false;
    };
    address >= base && end <= buffer_end
}

impl FileLike for Card1File {
    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        self.base.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        self.base.write(src)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        self.base.stat()
    }

    fn path(&self) -> Cow<'_, str> {
        self.base.path()
    }

    fn ioctl(
        &self,
        current: &UserTaskRef,
        cmd: u32,
        arg: usize,
    ) -> StarryResult<usize> {
        if arg == 0 {
            return Err(StarryError::BadAddress);
        }
        let _operation = self.operation.lock();
        retry_deferred_gem_destroys();
        let nr = ioctl_nr(cmd);
        info!("card1: cmd {cmd:#x}, nr {nr:#x}, arg {arg:#x}");
        if is_driver_ioctl(nr) {
            let op = RknpuCmd::try_from(nr).map_err(|_| StarryError::NotATty)?;
            return Ok(self.rknpu_driver_ioctl(current, op, arg)?);
        }

        if nr > MAX_IOCTL_NR {
            return Err(StarryError::NotATty);
        }
        let mut stack_data = AlignedIoctlData([0u8; STACK_DATA_SIZE]);
        let in_size = io_size(cmd) as usize;
        if in_size > stack_data.0.len() {
            return Err(StarryError::InvalidInput);
        }
        copy_from_user(current, &mut stack_data.0[..in_size], arg)?;
        match nr {
            DRM_IOCTL_VERSION_NR => drm_version(current, &mut stack_data.0)?,
            DRM_IOCTL_GET_UNIQUE_NR => drm_get_unique(&mut stack_data.0)?,
            DRM_IOCTL_GEM_FLINK_NR => {
                drm_gem_flink_ioctl(&mut stack_data.0)?;
            }
            DRM_IOCTL_PRIME_HANDLE_TO_FD_NR => {
                self.drm_prime_handle_to_fd_ioctl(&mut stack_data.0)?;
            }
            DRM_IOCTL_PRIME_FD_TO_HANDLE_NR => {
                self.drm_prime_fd_to_handle_ioctl(&mut stack_data.0)?;
            }
            _ => return Err(VfsError::NotATty.into()),
        }
        copy_to_user(current, arg, &stack_data.0[..in_size])?;
        Ok(0)
    }

    fn device_mmap(&self, offset: u64, _length: u64) -> StarryResult<DeviceMmap> {
        let _operation = self.operation.lock();
        retry_deferred_gem_destroys();
        let handle = map_handle_from_offset(offset).ok_or(StarryError::InvalidInput)?;
        Ok(self.exported_gem_buffer(handle)?.device_mmap_kind_resolved())
    }

    fn open_flags(&self) -> u32 {
        self.base.open_flags()
    }

    fn nonblocking(&self) -> bool {
        self.base.nonblocking()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> StarryResult {
        self.base.set_nonblocking(nonblocking)
    }
}

impl Pollable for Card1File {
    /// The engine is driven synchronously inside `ioctl`, so the fd is always ready.
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}

impl Card1File {
    fn rknpu_driver_ioctl(
        &self,
        current: &UserTaskRef,
        op: RknpuCmd,
        arg: usize,
    ) -> VfsResult<usize> {
        info!("rknpu_driver_ioctl: op = {:?}", op);
        match op {
            RknpuCmd::Submit => {
                let mut submit_args = RknpuSubmit::default();
                copy_from_user(
                    current,
                    bytemuck::bytes_of_mut(&mut submit_args),
                    arg,
                )?;
                let submit_result = self.handle_submit(current, &mut submit_args);
                copy_to_user(
                    current,
                    arg,
                    bytemuck::bytes_of(&submit_args),
                )?;
                submit_result?;
            }
            RknpuCmd::MemCreate => {
                let mut mem_create_args = RknpuMemCreate::default();
                copy_from_user(
                    current,
                    bytemuck::bytes_of_mut(&mut mem_create_args),
                    arg,
                )?;
                rknpu::mem_create(&mut mem_create_args).map_err(map_rknpu_err)?;
                let global_handle = mem_create_args.handle;
                let local_handle = match self.add_handle(global_handle) {
                    Ok(handle) => handle,
                    Err(error) => {
                        destroy_gem_or_defer(global_handle);
                        return Err(error);
                    }
                };
                mem_create_args.handle = local_handle;
                if let Err(error) = copy_to_user(
                    current,
                    arg,
                    bytemuck::bytes_of(&mem_create_args),
                ) {
                    let global_handle = self.remove_handle(local_handle)?;
                    destroy_gem_or_defer(global_handle);
                    return Err(error);
                }
            }
            RknpuCmd::MemMap => {
                let mut mem_map = RknpuMemMap::default();
                copy_from_user(
                    current,
                    bytemuck::bytes_of_mut(&mut mem_map),
                    arg,
                )?;
                self.global_handle(mem_map.handle)?;
                mem_map.offset = (mem_map.handle as u64) << PAGE_SHIFT;
                copy_to_user(
                    current,
                    arg,
                    bytemuck::bytes_of(&mem_map),
                )?;
            }
            RknpuCmd::MemDestroy => {
                let mut mem_destroy = RknpuMemDestroy::default();
                copy_from_user(
                    current,
                    bytemuck::bytes_of_mut(&mut mem_destroy),
                    arg,
                )?;
                let global_handle = self.global_handle(mem_destroy.handle)?;
                rknpu::mem_destroy(global_handle).map_err(map_rknpu_err)?;
                self.remove_handle(mem_destroy.handle)?;
            }
            RknpuCmd::MemSync => {
                let mut mem_sync = RknpuMemSync::default();
                copy_from_user(
                    current,
                    bytemuck::bytes_of_mut(&mut mem_sync),
                    arg,
                )?;
                if !self.find_cpu_range(mem_sync.obj_addr, mem_sync.offset, mem_sync.size) {
                    return Err(VfsError::InvalidData);
                }
                rknpu::mem_sync(&mut mem_sync).map_err(map_rknpu_err)?;
                copy_to_user(
                    current,
                    arg,
                    bytemuck::bytes_of(&mem_sync),
                )?;
            }
            RknpuCmd::Action => {
                let mut action = RknpuUserAction { flags: 0, value: 0 };
                copy_from_user(
                    current,
                    bytemuck::bytes_of_mut(&mut action),
                    arg,
                )?;
                let action_kind = decode_rknpu_action(action.flags)?;
                action.value = rknpu::action(action_kind).map_err(map_rknpu_err)?;
                copy_to_user(
                    current,
                    arg,
                    bytemuck::bytes_of(&action),
                )?;
            }
        }
        Ok(0)
    }

    fn drm_prime_handle_to_fd_ioctl(&self, data: &mut [u8]) -> VfsResult<usize> {
        let data = unsafe { &mut *(data.as_mut_ptr() as *mut DrmPrimeHande) };
        let exported = self.exported_gem_buffer(data.handle).map_err(|_| VfsError::NotFound)?;
        data.fd = exported
            .add_to_fd_table(prime_fd_cloexec(data.flags))
            .map_err(|_| VfsError::NoMemory)?;
        Ok(0)
    }

    fn drm_prime_fd_to_handle_ioctl(&self, data: &mut [u8]) -> VfsResult<usize> {
        let req = unsafe { &mut *(data.as_mut_ptr() as *mut DrmPrimeHande) };
        let buf = resolve_contiguous_dmabuf(req.fd).ok_or(VfsError::InvalidInput)?;
        let global_handle = rknpu::mem_import(
            buf.dma_phys_base() as u64,
            buf.dma_cpu_base().ok_or(VfsError::InvalidInput)?,
            buf.dma_size(),
            0,
            buf.dma_retainer(),
        )
        .map_err(map_rknpu_err)?;
        req.handle = match self.add_handle(global_handle) {
            Ok(handle) => handle,
            Err(error) => {
                destroy_gem_or_defer(global_handle);
                return Err(error);
            }
        };
        Ok(0)
    }
}

impl Drop for Card1File {
    fn drop(&mut self) {
        let handles = core::mem::take(&mut *self.handles.lock());
        for global_handle in handles.into_values() {
            destroy_gem_or_defer(global_handle);
        }
    }
}

fn enqueue_deferred_gem_destroy(pending: &mut Vec<u32>, handle: u32) {
    if !pending.contains(&handle) {
        pending.push(handle);
    }
}

fn defer_gem_destroy(handle: u32) {
    enqueue_deferred_gem_destroy(&mut DEFERRED_GEM_DESTROYS.lock(), handle);
}

fn destroy_gem_or_defer(handle: u32) {
    match rknpu::mem_destroy(handle) {
        Ok(()) => {}
        Err(error @ (rknpu::Error::Busy | rknpu::Error::Quarantined)) => {
            warn!(
                "rknpu: GEM destroy for handle {} deferred after {:?}",
                handle, error
            );
            defer_gem_destroy(handle);
        }
        Err(error) => {
            warn!(
                "rknpu: GEM destroy for handle {} could not be completed: {:?}",
                handle, error
            );
        }
    }
}

fn retry_deferred_gem_destroys() {
    let pending = core::mem::take(&mut *DEFERRED_GEM_DESTROYS.lock());
    for handle in pending {
        match rknpu::mem_destroy(handle) {
            Ok(()) => {}
            Err(error @ (rknpu::Error::Busy | rknpu::Error::Quarantined)) => {
                warn!(
                    "rknpu: deferred GEM destroy for handle {} still unavailable: {:?}",
                    handle, error
                );
                defer_gem_destroy(handle);
            }
            Err(error) => {
                warn!(
                    "rknpu: deferred GEM destroy for handle {} was discarded: {:?}",
                    handle, error
                );
            }
        }
    }
}

struct ExportedGemBuffer {
    range: PhysAddrRange,
    cache_policy: GemCachePolicy,
    /// Keeps the backing GEM allocation alive for as long as a mapping derived
    /// from this buffer exists, so a `MemDestroy` (or a closed source dma-buf fd)
    /// cannot free pages that are still mapped (use-after-free guard).
    retainer: Arc<dyn Any + Send + Sync>,
}

impl ExportedGemBuffer {
    fn new(
        range: PhysAddrRange,
        cache_policy: GemCachePolicy,
        retainer: Arc<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            range,
            cache_policy,
            retainer,
        }
    }

    fn device_mmap_kind(&self) -> DeviceMmap {
        // Anchor the mapping to the backing allocation so it cannot be freed while
        // still mapped.
        let anchor = Some(self.retainer.clone());
        match self.cache_policy {
            GemCachePolicy::Cacheable => DeviceMmap::PhysicalCached(self.range, anchor),
            // Starry does not expose a write-combine PTE mode yet; keep it
            // non-cacheable instead of accidentally upgrading it to cached.
            GemCachePolicy::NonCacheable | GemCachePolicy::WriteCombine => {
                DeviceMmap::Physical(self.range, anchor)
            }
        }
    }

    /// Returns a mapping whose physical range has already been selected by the
    /// GEM handle encoded in the file offset. The mmap layer must not add that
    /// selector offset to the physical address a second time.
    fn device_mmap_kind_resolved(&self) -> DeviceMmap {
        let anchor = Some(self.retainer.clone());
        match self.cache_policy {
            GemCachePolicy::Cacheable => {
                DeviceMmap::PhysicalCachedResolved(self.range, anchor)
            }
            GemCachePolicy::NonCacheable | GemCachePolicy::WriteCombine => {
                DeviceMmap::PhysicalResolved(self.range, anchor)
            }
        }
    }
}

impl FileLike for ExportedGemBuffer {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[rknpu-gem]".into()
    }

    fn device_mmap(&self, _offset: u64, _length: u64) -> StarryResult<DeviceMmap> {
        Ok(self.device_mmap_kind())
    }
}

impl Pollable for ExportedGemBuffer {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}

fn prime_fd_cloexec(flags: u32) -> bool {
    flags & O_CLOEXEC != 0
}

fn map_handle_from_offset(offset: u64) -> Option<u32> {
    if offset & ((1 << PAGE_SHIFT) - 1) != 0 {
        return None;
    }
    let handle = u32::try_from(offset >> PAGE_SHIFT).ok()?;
    (handle != 0).then_some(handle)
}

fn exported_gem_buffer(handle: u32) -> StarryResult<ExportedGemBuffer> {
    let info = rknpu::buffer_info(handle)
        .map_err(map_rknpu_err)
        .map_err(|_| StarryError::NotFound)?;
    // The NPU runs IOMMU-bypassed, so the GEM buffer's `dma_addr` is its physical
    // base. Use it directly instead of `virt_to_phys(obj_addr)` so imported
    // buffers (whose CPU va is not necessarily in the linear map) map correctly;
    // for owned coherent buffers `dma_addr == phys(obj_addr)`, so this is
    // equivalent.
    // Hold a retainer for the backing allocation so a mapping of this handle
    // survives a concurrent MemDestroy / source-fd close without dangling.
    let retainer = rknpu::buffer_retainer(handle)
        .map_err(map_rknpu_err)
        .map_err(|_| StarryError::NotFound)?;
    let paddr = PhysAddr::from(info.dma_addr as usize);
    Ok(ExportedGemBuffer::new(
        PhysAddrRange::from_start_size(paddr, info.size),
        info.cache_policy,
        retainer,
    ))
}

fn map_rknpu_err(err: rknpu::Error) -> VfsError {
    match err {
        rknpu::Error::NotFound => VfsError::NotFound,
        rknpu::Error::Busy => VfsError::AlreadyExists,
        rknpu::Error::TimedOut => VfsError::TimedOut,
        rknpu::Error::Quarantined => VfsError::Io,
        rknpu::Error::InvalidData => VfsError::InvalidData,
    }
}

/// Copies data from user space to a kernel-owned byte slice.
fn copy_from_user(current: &UserTaskRef, dst: &mut [u8], src: usize) -> Result<(), VfsError> {
    let bytes = vm_load(current, src as *const u8, dst.len()).map_err(|err| {
        warn!("[rknpu]: copy_from_user failed: {err:?}");
        VfsError::BadAddress
    })?;
    dst.copy_from_slice(&bytes);
    Ok(())
}

/// Copies a kernel-owned byte slice to user space.
fn copy_to_user(current: &UserTaskRef, dst: usize, src: &[u8]) -> Result<(), VfsError> {
    vm_write_slice(current, dst as *mut u8, src).map_err(|err| {
        warn!("[rknpu]: copy_to_user failed: {err:?}");
        VfsError::BadAddress
    })
}

/// DRM_IOCTL_GEM_FLINK ioctl argument type
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct DrmGemFlink {
    /// GEM handle
    handle: u32,
    /// GEM name
    name: u32,
}

/// Handles DRM GEM flink ioctl command
fn drm_gem_flink_ioctl(data: &mut [u8]) -> VfsResult<usize> {
    let data = unsafe { &mut *(data.as_mut_ptr() as *mut DrmGemFlink) };
    info!("drm_gem_flink_ioctl called: {:#?}", data);
    Err(VfsError::NotFound)
}

/// DRM prime handle structure
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct DrmPrimeHande {
    /// Handle
    handle: u32,
    /// Flags
    flags: u32,
    /// File descriptor
    fd: i32,
}

/// Rust implementation of Linux kernel's drm_copy_field function
///
/// This function safely copies a string value to user space buffer,
/// similar to the Linux kernel implementation with proper error handling.
///
/// # Safety
///
/// The caller must provide a valid mutable kernel reference for `buf_len` and
/// a valid, initialized, NUL-terminated kernel string at `value`. `buf` is a
/// user address and is only passed to the checked user-memory writer.
unsafe fn drm_copy_field(
    current: &UserTaskRef,
    buf: *mut u8,
    buf_len: &mut usize,
    value: *const u8,
) -> Result<(), VfsError> {
    // Handle NULL value case - same as kernel's WARN_ONCE check
    if value.is_null() {
        warn!("[drm_copy_field] BUG: the value to copy was not set!");
        *buf_len = 0;
        return Ok(());
    }

    // Calculate actual string length using C string semantics
    let mut len = 0;
    unsafe {
        let mut ptr = value;
        while *ptr != 0 {
            len += 1;
            ptr = ptr.add(1);
        }
    }

    // Get the original buffer size
    let original_buf_len = *buf_len;

    // Update user's buffer length with actual string length (same as kernel)
    *buf_len = len;

    // Don't overflow user buffer - limit copy to available space
    let copy_len = if len > original_buf_len {
        original_buf_len
    } else {
        len
    };

    // Finally, try filling in the userbuf (same logic as kernel)
    if copy_len > 0 && !buf.is_null() {
        // SAFETY: `value` points to a NUL-terminated kernel string and the scan
        // above established that `copy_len` bytes are within that string.
        let value = unsafe { core::slice::from_raw_parts(value, copy_len) };
        copy_to_user(current, buf as usize, value)?;
    }

    Ok(())
}

/// Sets the DRM version information for the device
pub fn drm_version(current: &UserTaskRef, data: &mut [u8]) -> VfsResult<()> {
    let data = unsafe { &mut *(data.as_mut_ptr() as *mut DrmVersion) };
    info!("drm_version called: {:?}", data);

    // Set version information
    data.version_major = 0;
    data.version_minor = 9;
    data.version_patchlevel = 8;

    // Use drm_copy_field to handle string copying properly
    unsafe {
        // Copy driver name
        let ret = drm_copy_field(
            current,
            data.name as *mut u8,
            &mut data.name_len,
            DRM1_NAME.as_ptr().cast(),
        );
        if let Err(e) = ret {
            warn!("[drm_version] Failed to copy driver name: {:?}", e);
            return Err(VfsError::InvalidData);
        }

        // Copy driver date
        let ret = drm_copy_field(
            current,
            data.date as *mut u8,
            &mut data.date_len,
            DRM1_DATE.as_ptr().cast(),
        );
        if let Err(e) = ret {
            warn!("[drm_version] Failed to copy driver date: {:?}", e);
            return Err(VfsError::InvalidData);
        }

        // Copy driver description
        let ret = drm_copy_field(
            current,
            data.desc as *mut u8,
            &mut data.desc_len,
            DRM1_DESC.as_ptr().cast(),
        );
        if let Err(e) = ret {
            warn!("[drm_version] Failed to copy driver description: {:?}", e);
            return Err(VfsError::InvalidData);
        }
    }

    info!(
        "[drm_version] Set driver info: name_len={}, date_len={}, desc_len={}",
        data.name_len, data.date_len, data.desc_len
    );

    Ok(())
}

/// DRM_GET_UNIQUE ioctl handler
///
/// This function handles DRM_IOCTL_GET_UNIQUE requests, returning the unique
/// identifier for the DRM device (typically a bus ID or similar identifier).
pub fn drm_get_unique(data: &mut [u8]) -> VfsResult<()> {
    let unique_data = unsafe { &mut *(data.as_mut_ptr() as *mut DrmUnique) };
    info!("drm_get_unique called: {:?}", unique_data);

    unique_data.unique_len = 0;

    Ok(())
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use ax_memory_addr::PhysAddrRange;

    use super::*;

    #[test]
    fn prime_export_honors_cloexec_flag() {
        assert!(prime_fd_cloexec(linux_raw_sys::general::O_CLOEXEC as u32));
        assert!(!prime_fd_cloexec(0));
    }

    #[test]
    fn mem_map_offset_decodes_to_handle() {
        assert_eq!(map_handle_from_offset(0x1000), Some(1));
        assert_eq!(map_handle_from_offset(0x2000), Some(2));
        assert_eq!(map_handle_from_offset(0), None);
        assert_eq!(map_handle_from_offset(0x1001), None);
    }

    #[test]
    fn exported_buffer_reports_physical_device_mmap() {
        let range = PhysAddrRange::from_start_size(0x1234_5000.into(), 0x4000);
        let exported = ExportedGemBuffer::new(range, GemCachePolicy::Cacheable, Arc::new(()));

        assert!(
            matches!(exported.device_mmap(0, 0).unwrap(), DeviceMmap::PhysicalCached(actual, Some(_)) if actual == range)
        );
    }

    #[test]
    fn exported_buffer_defaults_to_uncached_device_mmap() {
        let range = PhysAddrRange::from_start_size(0x1234_5000.into(), 0x4000);
        let exported = ExportedGemBuffer::new(range, GemCachePolicy::NonCacheable, Arc::new(()));

        assert!(
            matches!(exported.device_mmap(0, 0).unwrap(), DeviceMmap::Physical(actual, Some(_)) if actual == range)
        );
    }

    #[test]
    fn exported_buffer_maps_write_combine_as_uncached_without_wc_pte_support() {
        let range = PhysAddrRange::from_start_size(0x1234_5000.into(), 0x4000);
        let exported = ExportedGemBuffer::new(range, GemCachePolicy::WriteCombine, Arc::new(()));

        assert!(
            matches!(exported.device_mmap(0, 0).unwrap(), DeviceMmap::Physical(actual, Some(_)) if actual == range)
        );
    }

    #[test]
    fn card1_selector_mmap_resolves_the_range_without_shifting_it() {
        let range = PhysAddrRange::from_start_size(0x1234_5000.into(), 0x4000);
        let exported = ExportedGemBuffer::new(range, GemCachePolicy::Cacheable, Arc::new(()));

        assert!(matches!(
            exported.device_mmap_kind_resolved(),
            DeviceMmap::PhysicalCachedResolved(actual, Some(_)) if actual == range
        ));
    }

    #[test]
    fn dma_range_requires_full_containment() {
        assert!(range_contains(0x1000, 0x1000, 0x1800, 0x800));
        assert!(!range_contains(0x1000, 0x1000, 0x2000, 1));
        assert!(!range_contains(0x1000, 0x1000, 0x3000, 1));
    }

    #[test]
    fn dma_range_rejects_integer_wraparound() {
        assert!(!range_contains(u64::MAX - 1, 4, u64::MAX - 1, 4));
        assert!(!range_contains(0x1000, 0x1000, u64::MAX - 1, 4));
    }

    #[test]
    fn submit_task_span_rejects_empty_and_unbounded_ranges() {
        assert!(Card1File::submit_task_span(&RknpuSubmit::default()).is_err());

        let mut args = RknpuSubmit {
            task_start: u32::MAX,
            task_number: u32::MAX,
            ..RknpuSubmit::default()
        };
        assert!(Card1File::submit_task_span(&args).is_err());

        args.task_start = 0;
        args.task_number = 4096;
        assert!(Card1File::submit_task_span(&args).is_err());
    }

    #[test]
    fn deferred_destroy_queue_keeps_each_handle_once() {
        let mut pending = Vec::new();
        enqueue_deferred_gem_destroy(&mut pending, 7);
        enqueue_deferred_gem_destroy(&mut pending, 7);
        enqueue_deferred_gem_destroy(&mut pending, 9);

        assert_eq!(pending.as_slice(), &[7, 9]);
    }
}
