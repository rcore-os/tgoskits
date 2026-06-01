use core::{
    any::Any,
    mem::{MaybeUninit, size_of},
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use ax_memory_addr::{PhysAddr, PhysAddrRange};
use ax_runtime::hal::cpu::asm::user_copy;
use ax_task::WaitQueue;
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use k230_kpu::{
    CommandRange, KPU_CFG_PADDR, KPU_CFG_SIZE, KPU_INFO_F_FAKE_OUTPUT, KPU_INFO_F_FDT,
    KPU_INFO_F_IRQ_WAIT, KPU_INFO_F_RUNTIME_SCRATCH, KPU_IOC_CLEAR, KPU_IOC_GET_INFO,
    KPU_IOC_GET_IRQ_COUNT, KPU_IOC_GET_STATUS, KPU_IOC_PROGRAM_COMMAND, KPU_IOC_RUN, KPU_IOC_START,
    KPU_IOC_WAIT_DONE, KPU_IRQ_NONE, KPU_L2_PADDR, KPU_L2_SIZE, KPU_MMAP_CFG_OFFSET,
    KPU_MMAP_FAKE_OUTPUT_OFFSET, KPU_MMAP_L2_OFFSET, KPU_MMAP_RUNTIME_COMMAND_OFFSET,
    KPU_MMAP_RUNTIME_DDR_OFFSET, KPU_MMAP_RUNTIME_DIRECT_IO_OFFSET, KPU_MMAP_RUNTIME_RDATA_OFFSET,
    Kpu, KpuInfo,
};

use crate::pseudofs::{DeviceMmap, DeviceOps};

pub const KPU_DEVICE_ID: DeviceId = DeviceId::new(240, 1);
const KPU_IRQ_WAIT_TIMEOUT: Duration = Duration::from_millis(100);
static KPU_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
static KPU_DONE_WQ: WaitQueue = WaitQueue::new();

pub struct KpuDevice {
    hw: Kpu,
    resource: KpuResource,
    irq_registered: bool,
}

impl KpuDevice {
    pub fn probe() -> Option<Self> {
        let resource = KpuResource::probe()?;
        let base_vaddr =
            match axklib::mem::iomap(PhysAddr::from(resource.cfg_paddr), resource.cfg_size) {
                Ok(base) => base.as_usize(),
                Err(err) => {
                    warn!(
                        "k230-kpu devfs: failed to map CFG MMIO at {:#x}+{:#x}: {err:?}",
                        resource.cfg_paddr, resource.cfg_size
                    );
                    return None;
                }
            };
        let irq_registered = resource.irq.map(register_kpu_irq).unwrap_or(false);
        info!(
            "k230-kpu devfs: cfg=[{:#x}, +{:#x}) l2=[{:#x}, +{:#x}) fake_output={:?} \
             runtime_scratch={} irq={:?} irq_wait={} source={}",
            resource.cfg_paddr,
            resource.cfg_size,
            resource.l2_paddr,
            resource.l2_size,
            resource.fake_output_range(),
            resource.runtime_scratch_available(),
            resource.irq,
            irq_registered,
            if resource.from_fdt { "fdt" } else { "static" }
        );
        Some(Self {
            hw: unsafe { Kpu::new(base_vaddr) },
            resource,
            irq_registered,
        })
    }

    fn copy_command_range(arg: usize) -> VfsResult<CommandRange> {
        copy_from_user(arg)
    }

    fn info(&self) -> KpuInfo {
        self.resource.info(self.irq_registered)
    }

    fn wait_done(&self, poll_limit: usize) -> VfsResult<()> {
        if self.hw.is_done() {
            return Ok(());
        }
        if self.irq_registered {
            let timed_out =
                KPU_DONE_WQ.wait_timeout_until(KPU_IRQ_WAIT_TIMEOUT, || self.hw.is_done());
            if !timed_out {
                return Ok(());
            }
        }
        self.hw
            .wait_done(poll_limit)
            .map_err(|_| VfsError::TimedOut)
    }
}

impl DeviceOps for KpuDevice {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        if buf.len() < size_of::<u32>() || !offset.is_multiple_of(size_of::<u32>() as u64) {
            return Err(VfsError::InvalidInput);
        }
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        if offset + size_of::<u32>() > self.resource.cfg_size {
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
        if offset + size_of::<u32>() > self.resource.cfg_size {
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
            KPU_IOC_GET_INFO => {
                let info = self.info();
                copy_to_user(arg, &info)?;
                Ok(0)
            }
            KPU_IOC_GET_IRQ_COUNT => {
                let count = KPU_IRQ_COUNT.load(Ordering::Acquire);
                copy_to_user(arg, &count)?;
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
                self.wait_done(poll_limit)?;
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
            KPU_MMAP_CFG_OFFSET if self.resource.cfg_size != 0 => DeviceMmap::Physical(
                PhysAddrRange::from_start_size(
                    PhysAddr::from(self.resource.cfg_paddr),
                    length.min(self.resource.cfg_size),
                ),
                None,
            ),
            offset if offset == self.resource.cfg_paddr as u64 && self.resource.cfg_size != 0 => {
                DeviceMmap::Physical(
                    PhysAddrRange::from_start_size(
                        PhysAddr::from(self.resource.cfg_paddr),
                        length.min(self.resource.cfg_size),
                    ),
                    None,
                )
            }
            KPU_MMAP_L2_OFFSET if self.resource.l2_size != 0 => DeviceMmap::Physical(
                PhysAddrRange::from_start_size(
                    PhysAddr::from(self.resource.l2_paddr),
                    length.min(self.resource.l2_size),
                ),
                None,
            ),
            offset if offset == self.resource.l2_paddr as u64 && self.resource.l2_size != 0 => {
                DeviceMmap::Physical(
                    PhysAddrRange::from_start_size(
                        PhysAddr::from(self.resource.l2_paddr),
                        length.min(self.resource.l2_size),
                    ),
                    None,
                )
            }
            KPU_MMAP_FAKE_OUTPUT_OFFSET if self.resource.fake_output_size != 0 => {
                DeviceMmap::Physical(
                    PhysAddrRange::from_start_size(
                        PhysAddr::from(self.resource.fake_output_paddr),
                        length.min(self.resource.fake_output_size),
                    ),
                    None,
                )
            }
            KPU_MMAP_RUNTIME_RDATA_OFFSET if self.resource.runtime_rdata_size != 0 => {
                DeviceMmap::Physical(
                    PhysAddrRange::from_start_size(
                        PhysAddr::from(self.resource.runtime_rdata_paddr),
                        length.min(self.resource.runtime_rdata_size),
                    ),
                    None,
                )
            }
            KPU_MMAP_RUNTIME_COMMAND_OFFSET if self.resource.runtime_command_size != 0 => {
                DeviceMmap::Physical(
                    PhysAddrRange::from_start_size(
                        PhysAddr::from(self.resource.runtime_command_paddr),
                        length.min(self.resource.runtime_command_size),
                    ),
                    None,
                )
            }
            KPU_MMAP_RUNTIME_DIRECT_IO_OFFSET if self.resource.runtime_direct_io_size != 0 => {
                DeviceMmap::Physical(
                    PhysAddrRange::from_start_size(
                        PhysAddr::from(self.resource.runtime_direct_io_paddr),
                        length.min(self.resource.runtime_direct_io_size),
                    ),
                    None,
                )
            }
            KPU_MMAP_RUNTIME_DDR_OFFSET if self.resource.runtime_ddr_size != 0 => {
                DeviceMmap::Physical(
                    PhysAddrRange::from_start_size(
                        PhysAddr::from(self.resource.runtime_ddr_paddr),
                        length.min(self.resource.runtime_ddr_size),
                    ),
                    None,
                )
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

#[derive(Clone, Copy)]
struct KpuResource {
    cfg_paddr: usize,
    cfg_size: usize,
    l2_paddr: usize,
    l2_size: usize,
    fake_output_paddr: usize,
    fake_output_size: usize,
    runtime_rdata_paddr: usize,
    runtime_rdata_size: usize,
    runtime_command_paddr: usize,
    runtime_command_size: usize,
    runtime_direct_io_paddr: usize,
    runtime_direct_io_size: usize,
    runtime_ddr_paddr: usize,
    runtime_ddr_size: usize,
    irq: Option<usize>,
    from_fdt: bool,
}

impl KpuResource {
    fn probe() -> Option<Self> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "plat-dyn")] {
                let resource = Self::from_fdt();
                if resource.is_none() {
                    warn!("k230-kpu devfs: canaan,k230-kpu node not found in FDT");
                }
                resource
            } else {
                Some(Self::fallback(false))
            }
        }
    }

    fn fallback(from_fdt: bool) -> Self {
        Self {
            cfg_paddr: KPU_CFG_PADDR,
            cfg_size: KPU_CFG_SIZE,
            l2_paddr: KPU_L2_PADDR,
            l2_size: KPU_L2_SIZE,
            fake_output_paddr: 0,
            fake_output_size: 0,
            runtime_rdata_paddr: 0,
            runtime_rdata_size: 0,
            runtime_command_paddr: 0,
            runtime_command_size: 0,
            runtime_direct_io_paddr: 0,
            runtime_direct_io_size: 0,
            runtime_ddr_paddr: 0,
            runtime_ddr_size: 0,
            irq: Some(k230_kpu::KPU_IRQ),
            from_fdt,
        }
    }

    #[cfg(feature = "plat-dyn")]
    fn from_fdt() -> Option<Self> {
        rdrive::with_fdt(|fdt| {
            fdt.find_compatible(&["canaan,k230-kpu"])
                .into_iter()
                .find_map(Self::from_fdt_node)
        })
        .flatten()
    }

    #[cfg(feature = "plat-dyn")]
    fn from_fdt_node(node: rdrive::probe::fdt::NodeType<'_>) -> Option<Self> {
        if matches!(
            node.as_node().status(),
            Some(rdrive::probe::fdt::Status::Disabled)
        ) {
            return None;
        }

        let mut regs = node.regs().into_iter();
        let cfg_reg = regs.next()?;
        let l2_reg = regs.next();
        let fallback = Self::fallback(true);
        let fake_output = decode_fake_output_region(&node);
        let runtime_rdata = decode_named_region(
            &node,
            "canaan,qemu-runtime-rdata",
            "canaan,k230-kpu-qemu-runtime-rdata",
        );
        let runtime_command = decode_named_region(
            &node,
            "canaan,qemu-runtime-command",
            "canaan,k230-kpu-qemu-runtime-command",
        );
        let runtime_direct_io = decode_named_region(
            &node,
            "canaan,qemu-runtime-direct-io",
            "canaan,k230-kpu-qemu-runtime-direct-io",
        );
        let runtime_ddr = decode_named_region(
            &node,
            "canaan,qemu-runtime-ddr",
            "canaan,k230-kpu-qemu-runtime-ddr",
        );

        Some(Self {
            cfg_paddr: cfg_reg.address as usize,
            cfg_size: cfg_reg.size.unwrap_or(KPU_CFG_SIZE as u64) as usize,
            l2_paddr: l2_reg
                .as_ref()
                .map(|reg| reg.address as usize)
                .unwrap_or(fallback.l2_paddr),
            l2_size: l2_reg
                .and_then(|reg| reg.size)
                .map(|size| size as usize)
                .unwrap_or(fallback.l2_size),
            fake_output_paddr: fake_output
                .map(|(paddr, _size)| paddr)
                .unwrap_or(fallback.fake_output_paddr),
            fake_output_size: fake_output
                .map(|(_paddr, size)| size)
                .unwrap_or(fallback.fake_output_size),
            runtime_rdata_paddr: runtime_rdata
                .map(|(paddr, _size)| paddr)
                .unwrap_or(fallback.runtime_rdata_paddr),
            runtime_rdata_size: runtime_rdata
                .map(|(_paddr, size)| size)
                .unwrap_or(fallback.runtime_rdata_size),
            runtime_command_paddr: runtime_command
                .map(|(paddr, _size)| paddr)
                .unwrap_or(fallback.runtime_command_paddr),
            runtime_command_size: runtime_command
                .map(|(_paddr, size)| size)
                .unwrap_or(fallback.runtime_command_size),
            runtime_direct_io_paddr: runtime_direct_io
                .map(|(paddr, _size)| paddr)
                .unwrap_or(fallback.runtime_direct_io_paddr),
            runtime_direct_io_size: runtime_direct_io
                .map(|(_paddr, size)| size)
                .unwrap_or(fallback.runtime_direct_io_size),
            runtime_ddr_paddr: runtime_ddr
                .map(|(paddr, _size)| paddr)
                .unwrap_or(fallback.runtime_ddr_paddr),
            runtime_ddr_size: runtime_ddr
                .map(|(_paddr, size)| size)
                .unwrap_or(fallback.runtime_ddr_size),
            irq: decode_fdt_irq(&node.interrupts()).or(fallback.irq),
            from_fdt: true,
        })
    }

    fn fake_output_range(&self) -> Option<(usize, usize)> {
        (self.fake_output_size != 0).then_some((self.fake_output_paddr, self.fake_output_size))
    }

    fn runtime_scratch_available(&self) -> bool {
        self.runtime_rdata_size != 0
            && self.runtime_command_size != 0
            && self.runtime_direct_io_size != 0
            && self.runtime_ddr_size != 0
    }

    fn info(&self, irq_wait: bool) -> KpuInfo {
        KpuInfo {
            cfg_paddr: self.cfg_paddr as u64,
            cfg_size: self.cfg_size as u64,
            l2_paddr: self.l2_paddr as u64,
            l2_size: self.l2_size as u64,
            irq: self
                .irq
                .and_then(|irq| u32::try_from(irq).ok())
                .unwrap_or(KPU_IRQ_NONE),
            flags: (if self.from_fdt { KPU_INFO_F_FDT } else { 0 })
                | (if irq_wait { KPU_INFO_F_IRQ_WAIT } else { 0 })
                | (if self.fake_output_size != 0 {
                    KPU_INFO_F_FAKE_OUTPUT
                } else {
                    0
                })
                | (if self.runtime_scratch_available() {
                    KPU_INFO_F_RUNTIME_SCRATCH
                } else {
                    0
                }),
        }
    }
}

fn register_kpu_irq(irq: usize) -> bool {
    if ax_runtime::hal::irq::request_shared_irq(irq, kpu_irq_handler, NonNull::dangling()).is_err()
    {
        warn!("k230-kpu devfs: failed to register IRQ handler for irq {irq}");
        return false;
    }
    ax_runtime::hal::irq::set_enable(irq, true);
    true
}

unsafe fn kpu_irq_handler(
    _ctx: ax_runtime::hal::irq::IrqContext,
    _data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    KPU_IRQ_COUNT.fetch_add(1, Ordering::AcqRel);
    KPU_DONE_WQ.notify_all(false);
    ax_runtime::hal::irq::IrqReturn::Handled
}

#[cfg(feature = "plat-dyn")]
fn decode_fdt_irq(interrupts: &[rdrive::probe::fdt::InterruptRef]) -> Option<usize> {
    let interrupt = interrupts.first()?;
    match interrupt.specifier.as_slice() {
        [irq] => Some(*irq as usize),
        [kind, irq, ..] => match *kind {
            0 => Some(*irq as usize + 32),
            1 => Some(*irq as usize + 16),
            _ => Some(*irq as usize),
        },
        _ => None,
    }
}

#[cfg(feature = "plat-dyn")]
fn decode_fake_output_region(node: &rdrive::probe::fdt::NodeType<'_>) -> Option<(usize, usize)> {
    decode_named_region(node, "memory-region", "canaan,k230-kpu-qemu-fake-output")
}

#[cfg(feature = "plat-dyn")]
fn decode_named_region(
    node: &rdrive::probe::fdt::NodeType<'_>,
    prop_name: &str,
    compatible: &str,
) -> Option<(usize, usize)> {
    let phandle = node.as_node().get_property(prop_name)?.get_u32()?;
    rdrive::with_fdt(|fdt| {
        let region = fdt.get_by_phandle(rdrive::probe::fdt::Phandle::from(phandle))?;
        let supported = region
            .as_node()
            .compatibles()
            .any(|compat| compat == compatible);
        if !supported {
            return None;
        }
        let reg = region.regs().into_iter().next()?;
        let size = reg.size?;
        Some((reg.address as usize, size as usize))
    })
    .flatten()
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
