use alloc::{collections::VecDeque, string::String, sync::Arc};
use core::{
    any::Any,
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    time::Duration,
};

use ax_kspin::SpinNoIrq;
use ax_memory_addr::{PhysAddr, PhysAddrRange};
use ax_runtime::{
    hal::irq::IrqReturn,
    maintenance::{
        DeviceMaintenanceHandle, LocalIrqWake, LocalOwnerCell, LocalOwnerControl, LocalOwnerIrq,
        MaintenanceCauses, MaintenanceClosed, MaintenanceError, MaintenanceIrqAction,
        MaintenancePublishResult, MaintenanceRegistrar, MaintenanceSession, MaintenanceState,
        MaintenanceThread, spawn_maintenance_domain,
    },
};
use ax_std::os::arceos::task::WaitQueue;
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use bytemuck::{AnyBitPattern, NoUninit};
use k230_kpu::{
    CommandRange, KPU_CFG_PADDR, KPU_CFG_SIZE, KPU_INFO_F_FAKE_OUTPUT, KPU_INFO_F_FDT,
    KPU_INFO_F_IRQ_WAIT, KPU_INFO_F_RUNTIME_SCRATCH, KPU_IOC_CLEAR, KPU_IOC_GET_INFO,
    KPU_IOC_GET_IRQ_COUNT, KPU_IOC_GET_STATUS, KPU_IOC_PROGRAM_COMMAND, KPU_IOC_RUN, KPU_IOC_START,
    KPU_IOC_WAIT_DONE, KPU_IRQ_NONE, KPU_L2_PADDR, KPU_L2_SIZE, KPU_MMAP_CFG_OFFSET,
    KPU_MMAP_FAKE_OUTPUT_OFFSET, KPU_MMAP_L2_OFFSET, KPU_MMAP_RUNTIME_COMMAND_OFFSET,
    KPU_MMAP_RUNTIME_DDR_OFFSET, KPU_MMAP_RUNTIME_DIRECT_IO_OFFSET, KPU_MMAP_RUNTIME_RDATA_OFFSET,
    Kpu, KpuInfo,
};

use crate::{
    mm::{UserConstPtr, UserPtr},
    pseudofs::{DeviceMmap, DeviceOps},
};

pub const KPU_DEVICE_ID: DeviceId = DeviceId::new(240, 1);
const KPU_IRQ_WAIT_TIMEOUT: Duration = Duration::from_millis(100);
// K230 exposes one KPU instance. If a future platform exposes more instances,
// move this IRQ state into per-device storage.
static KPU_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
static KPU_DONE_WQ: WaitQueue = WaitQueue::new();
const KPU_MAINTENANCE_CPU: usize = 0;
const KPU_EVENT_BATCH_LIMIT: usize = 64;
const KPU_START_PENDING: u8 = 0;
const KPU_START_READY: u8 = 1;
const KPU_START_FAILED: u8 = 2;
const KPU_REQUEST_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug)]
enum KpuMaintenanceEvent {
    Completion,
}

#[derive(Clone, Copy, Debug)]
enum KpuOperation {
    ReadRegister { offset: usize },
    WriteRegister { offset: usize, value: u32 },
    Status,
    Clear,
    Program(CommandRange),
    Start,
    Run(CommandRange),
}

#[derive(Clone, Copy, Debug)]
enum KpuOperationResult {
    Unit,
    Register(u32),
    Status(u64),
    StartedAt(u64),
}

#[derive(Clone, Copy, Debug)]
enum KpuOperationError {
    InvalidCommand,
    OwnerUnavailable,
}

struct KpuRequestCompletion {
    result: SpinNoIrq<Option<Result<KpuOperationResult, KpuOperationError>>>,
    wait: WaitQueue,
}

impl KpuRequestCompletion {
    const fn new() -> Self {
        Self {
            result: SpinNoIrq::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn complete(&self, result: Result<KpuOperationResult, KpuOperationError>) {
        *self.result.lock() = Some(result);
        self.wait.notify_all();
    }

    fn wait(&self) -> Result<KpuOperationResult, KpuOperationError> {
        self.wait.wait_until(|| self.result.lock().is_some());
        self.result
            .lock()
            .take()
            .expect("KPU completion predicate guarantees one result")
    }
}

struct KpuRequest {
    operation: KpuOperation,
    completion: Arc<KpuRequestCompletion>,
}

struct KpuIngress {
    requests: SpinNoIrq<VecDeque<KpuRequest>>,
}

impl KpuIngress {
    const fn new() -> Self {
        Self {
            requests: SpinNoIrq::new(VecDeque::new()),
        }
    }
}

struct KpuMaintenanceStartup {
    state: AtomicU8,
    line_quenched: AtomicBool,
    uncontained: AtomicBool,
    wait: WaitQueue,
    remote: SpinNoIrq<Option<DeviceMaintenanceHandle<KpuMaintenanceEvent>>>,
}

impl KpuMaintenanceStartup {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(KPU_START_PENDING),
            line_quenched: AtomicBool::new(false),
            uncontained: AtomicBool::new(false),
            wait: WaitQueue::new(),
            remote: SpinNoIrq::new(None),
        }
    }

    fn publish_ready(&self, remote: DeviceMaintenanceHandle<KpuMaintenanceEvent>) {
        *self.remote.lock() = Some(remote);
        self.state.store(KPU_START_READY, Ordering::Release);
        self.wait.notify_all();
    }

    fn publish_failed(&self) {
        self.state.store(KPU_START_FAILED, Ordering::Release);
        self.wait.notify_all();
    }

    fn take_remote(&self) -> Option<DeviceMaintenanceHandle<KpuMaintenanceEvent>> {
        self.wait
            .wait_until(|| self.state.load(Ordering::Acquire) != KPU_START_PENDING);
        if self.state.load(Ordering::Acquire) != KPU_START_READY {
            return None;
        }
        self.remote.lock().take()
    }
}

struct KpuMaintenanceRuntime {
    remote: DeviceMaintenanceHandle<KpuMaintenanceEvent>,
    ingress: Arc<KpuIngress>,
    _thread: MaintenanceThread,
}

impl KpuMaintenanceRuntime {
    fn is_live(&self) -> bool {
        self.remote.state() == MaintenanceState::Live
    }

    fn execute(&self, operation: KpuOperation) -> Result<KpuOperationResult, KpuOperationError> {
        let completion = Arc::new(KpuRequestCompletion::new());
        let mut requests = self.ingress.requests.lock();
        if requests.len() == KPU_REQUEST_CAPACITY {
            return Err(KpuOperationError::OwnerUnavailable);
        }
        requests.push_back(KpuRequest {
            operation,
            completion: Arc::clone(&completion),
        });
        if self
            .remote
            .publish_cause(MaintenanceCauses::SUBMIT)
            .is_err()
        {
            let _rejected = requests.pop_back();
            return Err(KpuOperationError::OwnerUnavailable);
        }
        drop(requests);
        completion.wait()
    }
}

impl Drop for KpuMaintenanceRuntime {
    fn drop(&mut self) {
        let _ = self.remote.request_shutdown();
    }
}

pub struct KpuDevice {
    resource: KpuResource,
    maintenance: KpuMaintenanceRuntime,
    start_irq_generation: AtomicU64,
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
        let hw = unsafe { Kpu::new(base_vaddr) };
        let irq = resource.irq.or_else(|| {
            warn!("k230-kpu devfs: refusing to publish a polling-only KPU device");
            None
        })?;
        let maintenance = spawn_kpu_maintenance(irq, hw)?;
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
            maintenance.is_live(),
            if resource.from_fdt { "fdt" } else { "static" }
        );
        Some(Self {
            resource,
            maintenance,
            start_irq_generation: AtomicU64::new(KPU_IRQ_COUNT.load(Ordering::Acquire)),
        })
    }

    fn copy_command_range(arg: usize) -> VfsResult<CommandRange> {
        copy_from_user(arg)
    }

    fn info(&self) -> KpuInfo {
        self.resource.info(self.irq_maintenance_live())
    }

    fn wait_done(&self, _legacy_wait_hint: usize) -> VfsResult<()> {
        let generation = self.start_irq_generation.load(Ordering::Acquire);
        if KPU_IRQ_COUNT.load(Ordering::Acquire) != generation {
            return Ok(());
        }
        let timed_out = KPU_DONE_WQ.wait_timeout_until(KPU_IRQ_WAIT_TIMEOUT, || {
            KPU_IRQ_COUNT.load(Ordering::Acquire) != generation
        });
        if timed_out {
            Err(VfsError::TimedOut)
        } else {
            Ok(())
        }
    }

    fn irq_maintenance_live(&self) -> bool {
        self.maintenance.is_live()
    }

    fn execute(&self, operation: KpuOperation) -> VfsResult<KpuOperationResult> {
        self.maintenance
            .execute(operation)
            .map_err(|error| match error {
                KpuOperationError::InvalidCommand => VfsError::InvalidInput,
                KpuOperationError::OwnerUnavailable => VfsError::Io,
            })
    }

    fn record_start(&self, result: KpuOperationResult) -> VfsResult<()> {
        let KpuOperationResult::StartedAt(generation) = result else {
            return Err(VfsError::Io);
        };
        self.start_irq_generation
            .store(generation, Ordering::Release);
        Ok(())
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
        let KpuOperationResult::Register(value) =
            self.execute(KpuOperation::ReadRegister { offset })?
        else {
            return Err(VfsError::Io);
        };
        let value = value.to_ne_bytes();
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
        let result = self.execute(KpuOperation::WriteRegister {
            offset,
            value: u32::from_ne_bytes(bytes),
        })?;
        if !matches!(result, KpuOperationResult::Unit) {
            return Err(VfsError::Io);
        }
        Ok(size_of::<u32>())
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            KPU_IOC_GET_STATUS => {
                let KpuOperationResult::Status(status) = self.execute(KpuOperation::Status)? else {
                    return Err(VfsError::Io);
                };
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
                self.record_start(self.execute(KpuOperation::Clear)?)?;
                Ok(0)
            }
            KPU_IOC_PROGRAM_COMMAND => {
                let range = Self::copy_command_range(arg)?;
                self.execute(KpuOperation::Program(range))?;
                Ok(0)
            }
            KPU_IOC_START => {
                self.record_start(self.execute(KpuOperation::Start)?)?;
                Ok(0)
            }
            KPU_IOC_RUN => {
                let range = Self::copy_command_range(arg)?;
                self.record_start(self.execute(KpuOperation::Run(range))?)?;
                Ok(0)
            }
            KPU_IOC_WAIT_DONE => {
                // The historical argument was a userspace polling budget. It
                // remains ABI-compatible but completion now comes only from
                // the acknowledged IRQ generation or the absolute watchdog.
                self.wait_done(arg)?;
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
            KPU_MMAP_L2_OFFSET if self.resource.l2_size != 0 => DeviceMmap::Physical(
                PhysAddrRange::from_start_size(
                    PhysAddr::from(self.resource.l2_paddr),
                    length.min(self.resource.l2_size),
                ),
                None,
            ),
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
    irq: Option<ax_runtime::hal::irq::IrqId>,
    from_fdt: bool,
}

impl KpuResource {
    fn probe() -> Option<Self> {
        let resource = Self::from_fdt();
        if resource.is_none() {
            warn!("k230-kpu devfs: canaan,k230-kpu node not found in FDT");
        }
        resource
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
            irq: fallback_irq(),
            from_fdt,
        }
    }

    fn from_fdt() -> Option<Self> {
        rdrive::with_fdt(|fdt| {
            fdt.find_compatible(&["canaan,k230-kpu"])
                .into_iter()
                .find_map(Self::from_fdt_node)
        })
        .flatten()
    }

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
        let irq = match decode_fdt_irq(&node.interrupts()) {
            Ok(irq) => irq,
            Err(err) => {
                warn!("k230-kpu devfs: failed to resolve KPU IRQ: {err:?}");
                return None;
            }
        };

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
            irq,
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
            irq: self.irq.map(|irq| irq.hwirq.0).unwrap_or(KPU_IRQ_NONE),
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

fn spawn_kpu_maintenance(
    irq: ax_runtime::hal::irq::IrqId,
    hw: Kpu,
) -> Option<KpuMaintenanceRuntime> {
    let startup = Arc::new(KpuMaintenanceStartup::new());
    let ingress = Arc::new(KpuIngress::new());
    let owner_startup = Arc::clone(&startup);
    let owner_ingress = Arc::clone(&ingress);
    let thread = match spawn_maintenance_domain::<KpuMaintenanceEvent, _>(
        KPU_MAINTENANCE_CPU,
        String::from("kpu-maintenance"),
        move |registrar| run_kpu_maintenance(hw, irq, owner_startup, owner_ingress, registrar),
    ) {
        Ok(thread) => thread,
        Err(error) => {
            warn!("k230-kpu devfs: failed to spawn IRQ maintenance owner: {error}");
            return None;
        }
    };
    let remote = startup.take_remote()?;
    Some(KpuMaintenanceRuntime {
        remote,
        ingress,
        _thread: thread,
    })
}

fn run_kpu_maintenance(
    hw: Kpu,
    irq: ax_runtime::hal::irq::IrqId,
    startup: Arc<KpuMaintenanceStartup>,
    ingress: Arc<KpuIngress>,
    registrar: MaintenanceRegistrar<KpuMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    let owner_cell = LocalOwnerCell::pin(hw);
    let (owner, mut owner_irq) = registrar
        .local_owner_cell(owner_cell.as_ref())
        .map_err(|_| {
            startup.publish_failed();
            MaintenanceError::Irq(ax_runtime::hal::irq::IrqError::Unsupported)
        })?;
    let irq_wake = registrar
        .local_irq_wake()
        .inspect_err(|_| startup.publish_failed())?;
    let remote = registrar.remote_handle();
    let owner_cpu = registrar.owner_cpu();
    let callback_startup = Arc::clone(&startup);
    let registration = registrar.register_shared_disabled("k230-kpu", irq, move |context| {
        kpu_irq_action(
            context.cpu.0,
            owner_cpu,
            &irq_wake,
            &callback_startup,
            &mut owner_irq,
        )
    });
    let registration = match registration {
        Ok(registration) => registration,
        Err(error) => {
            warn!("k230-kpu devfs: failed to register IRQ {irq:?}: {error:?}");
            startup.publish_failed();
            let session = registrar.activate()?;
            let closed = close_kpu_session(session)?;
            owner_cell
                .reclaim(owner, &closed)
                .unwrap_or_else(|failure| {
                    panic!("failed to reclaim KPU owner: {}", failure.error())
                });
            return Ok(closed);
        }
    };
    let session = registrar
        .activate()
        .inspect_err(|_| startup.publish_failed())?;
    if let Err(error) = registration.enable() {
        warn!("k230-kpu devfs: failed to enable IRQ {irq:?}: {error:?}");
        startup.publish_failed();
        return close_kpu_maintenance(&startup, session, registration, owner_cell, owner);
    }
    startup.publish_ready(remote);
    info!("k230-kpu devfs: IRQ {irq:?} owned by maintenance CPU {owner_cpu}");

    let run_result = kpu_maintenance_loop(&startup, &ingress, &session, &registration, &owner);
    let close_result = close_kpu_maintenance(&startup, session, registration, owner_cell, owner);
    match close_result {
        Ok(closed) => {
            run_result?;
            Ok(closed)
        }
        Err(error) => Err(error),
    }
}

fn kpu_irq_action(
    actual_cpu: usize,
    owner_cpu: usize,
    wake: &LocalIrqWake<KpuMaintenanceEvent>,
    startup: &KpuMaintenanceStartup,
    owner_irq: &mut LocalOwnerIrq<Kpu>,
) -> IrqReturn {
    if actual_cpu != owner_cpu {
        startup.uncontained.store(true, Ordering::Release);
        startup.line_quenched.store(true, Ordering::Release);
        return IrqReturn::MaskLineAndWake;
    }
    let captured = owner_irq.with_irq(|hw| {
        if !hw.is_done() {
            return false;
        }
        hw.clear_done();
        true
    });
    if !matches!(captured, Ok(true)) {
        return if matches!(captured, Ok(false)) {
            IrqReturn::Unhandled
        } else {
            startup.uncontained.store(true, Ordering::Release);
            startup.line_quenched.store(true, Ordering::Release);
            IrqReturn::MaskLineAndWake
        };
    }
    match wake.publish_from_irq(MaintenanceCauses::IRQ, KpuMaintenanceEvent::Completion) {
        Ok(MaintenancePublishResult::Published) => IrqReturn::Wake,
        Ok(MaintenancePublishResult::Overflowed) | Err(_) => {
            startup.line_quenched.store(true, Ordering::Release);
            IrqReturn::MaskLineAndWake
        }
    }
}

fn kpu_maintenance_loop(
    startup: &KpuMaintenanceStartup,
    ingress: &KpuIngress,
    session: &MaintenanceSession<KpuMaintenanceEvent>,
    registration: &MaintenanceIrqAction,
    owner: &LocalOwnerControl<Kpu>,
) -> Result<(), MaintenanceError> {
    let mut run_again = false;
    let mut shutdown_requested = false;
    loop {
        if !run_again {
            session.wait_for_pending()?;
        }
        run_again = false;
        let mut completed = 0_u64;
        let drain = session.drain_owner(KPU_EVENT_BATCH_LIMIT, |event| match event {
            KpuMaintenanceEvent::Completion => completed += 1,
        })?;
        if completed != 0 {
            KPU_IRQ_COUNT.fetch_add(completed, Ordering::AcqRel);
            KPU_DONE_WQ.notify_all();
        }
        if drain.causes().contains(MaintenanceCauses::OVERFLOW) {
            return Err(MaintenanceError::Irq(
                ax_runtime::hal::irq::IrqError::NoMemory,
            ));
        }
        let requests_pending = service_kpu_requests(ingress, owner);
        shutdown_requested |= drain.causes().contains(MaintenanceCauses::SHUTDOWN);
        if startup.uncontained.load(Ordering::Acquire) {
            return Err(MaintenanceError::Irq(
                ax_runtime::hal::irq::IrqError::Unsupported,
            ));
        }
        if startup.line_quenched.load(Ordering::Acquire) {
            registration.release_quench()?;
            startup.line_quenched.store(false, Ordering::Release);
        }
        if shutdown_requested && !requests_pending {
            return Ok(());
        }
        if drain.pending() || requests_pending {
            crate::task::yield_now();
            if requests_pending {
                run_again = true;
            }
        }
    }
}

fn service_kpu_requests(ingress: &KpuIngress, owner: &LocalOwnerControl<Kpu>) -> bool {
    for _ in 0..KPU_REQUEST_CAPACITY {
        let Some(request) = ingress.requests.lock().pop_front() else {
            return false;
        };
        let result = owner
            .with_owner(|hw| execute_kpu_operation(hw, request.operation))
            .unwrap_or(Err(KpuOperationError::OwnerUnavailable));
        request.completion.complete(result);
    }
    !ingress.requests.lock().is_empty()
}

fn execute_kpu_operation(
    hw: &mut Kpu,
    operation: KpuOperation,
) -> Result<KpuOperationResult, KpuOperationError> {
    match operation {
        KpuOperation::ReadRegister { offset } => {
            Ok(KpuOperationResult::Register(hw.read_reg(offset)))
        }
        KpuOperation::WriteRegister { offset, value } => {
            hw.write_reg(offset, value);
            Ok(KpuOperationResult::Unit)
        }
        KpuOperation::Status => Ok(KpuOperationResult::Status(hw.status())),
        KpuOperation::Clear => {
            hw.clear_done();
            Ok(KpuOperationResult::StartedAt(
                KPU_IRQ_COUNT.load(Ordering::Acquire),
            ))
        }
        KpuOperation::Program(range) => hw
            .program_command(range)
            .map(|()| KpuOperationResult::Unit)
            .map_err(|_| KpuOperationError::InvalidCommand),
        KpuOperation::Start => {
            let generation = KPU_IRQ_COUNT.load(Ordering::Acquire);
            hw.start();
            Ok(KpuOperationResult::StartedAt(generation))
        }
        KpuOperation::Run(range) => {
            let generation = KPU_IRQ_COUNT.load(Ordering::Acquire);
            hw.run_command(range)
                .map(|()| KpuOperationResult::StartedAt(generation))
                .map_err(|_| KpuOperationError::InvalidCommand)
        }
    }
}

fn close_kpu_maintenance(
    startup: &KpuMaintenanceStartup,
    session: MaintenanceSession<KpuMaintenanceEvent>,
    registration: MaintenanceIrqAction,
    owner_cell: core::pin::Pin<alloc::boxed::Box<LocalOwnerCell<Kpu>>>,
    owner: LocalOwnerControl<Kpu>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    let begin_close = session.begin_close();
    owner
        .with_owner(|hw| hw.clear_done())
        .expect("KPU close must run in its owner domain");
    if startup.uncontained.load(Ordering::Acquire) {
        let _retained_registration = registration;
        session.quarantine_and_park();
    }
    if let Err(error) = registration.disable() {
        warn!("k230-kpu devfs: failed to disable IRQ action: {error:?}");
        let _retained_registration = registration;
        session.quarantine_and_park();
    }
    if startup.line_quenched.load(Ordering::Acquire) {
        if let Err(error) = registration.release_quench() {
            warn!("k230-kpu devfs: failed to release IRQ line quench: {error:?}");
            let _retained_registration = registration;
            session.quarantine_and_park();
        }
        startup.line_quenched.store(false, Ordering::Release);
    }
    if let Err(error) = registration.synchronize() {
        warn!("k230-kpu devfs: failed to synchronize IRQ action: {error:?}");
        let _retained_registration = registration;
        session.quarantine_and_park();
    }
    if let Err(failure) = registration.close() {
        let (reason, registration) = failure.into_parts();
        warn!("k230-kpu devfs: failed to destroy IRQ action: {reason:?}");
        let _retained_registration = registration;
        session.quarantine_and_park();
    }
    begin_close?;
    let closed = close_kpu_session(session)?;
    owner_cell
        .reclaim(owner, &closed)
        .unwrap_or_else(|failure| panic!("failed to reclaim KPU owner: {}", failure.error()));
    Ok(closed)
}

fn close_kpu_session(
    session: MaintenanceSession<KpuMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    if session.state() == MaintenanceState::Live {
        session.begin_close()?;
    }
    while session.state() == MaintenanceState::Closing {
        let drain = session.drain_owner(KPU_EVENT_BATCH_LIMIT, |_| {})?;
        if !drain.pending() {
            break;
        }
    }
    session.try_begin_draining()?;
    session.finish_close()?;
    session.try_into_closed().map_err(|failure| failure.error())
}

fn fallback_irq() -> Option<ax_runtime::hal::irq::IrqId> {
    None
}

fn decode_fdt_irq(
    interrupts: &[rdrive::probe::fdt::InterruptRef],
) -> Result<Option<ax_runtime::hal::irq::IrqId>, ax_runtime::hal::irq::IrqError> {
    let Some(interrupt) = interrupts.first() else {
        return Ok(None);
    };
    let controller = rdrive::fdt_phandle_to_device_id(interrupt.interrupt_parent)
        .ok_or(ax_runtime::hal::irq::IrqError::Unsupported)?;
    ax_runtime::irq::resolve_binding_irq(ax_driver::BindingIrq::fdt_interrupt_with_controller(
        controller,
        interrupt.specifier.clone(),
    ))
    .map(Some)
}

fn decode_fake_output_region(node: &rdrive::probe::fdt::NodeType<'_>) -> Option<(usize, usize)> {
    decode_named_region(node, "memory-region", "canaan,k230-kpu-qemu-fake-output")
}

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

fn copy_from_user<T: AnyBitPattern>(arg: usize) -> VfsResult<T> {
    if arg == 0 {
        return Err(VfsError::InvalidInput);
    }
    UserConstPtr::<T>::from(arg)
        .read()
        .map_err(|_| VfsError::InvalidData)
}

fn copy_to_user<T: NoUninit>(arg: usize, value: &T) -> VfsResult<()> {
    if arg == 0 {
        return Err(VfsError::InvalidInput);
    }
    UserPtr::<T>::from(arg)
        .write(*value)
        .map_err(|_| VfsError::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_wait_capability_is_published_only_for_live_maintenance() {
        let resource = KpuResource::fallback(false);
        assert_eq!(resource.info(false).flags & KPU_INFO_F_IRQ_WAIT, 0);
        assert_eq!(
            resource.info(true).flags & KPU_INFO_F_IRQ_WAIT,
            KPU_INFO_F_IRQ_WAIT
        );
    }

    #[test]
    fn hard_irq_batch_is_bounded_by_runtime_mailbox_capacity() {
        assert_eq!(KPU_EVENT_BATCH_LIMIT, 64);
    }
}
