//! TPU 设备 OS 适配
//!
//! 将 ioctl 命令翻译为 `Sg2002Tpu` 调用，并通过 fd 解析 Ion buffer
//! 物理/虚拟地址。
//!
//! 异步模型（复刻原 Linux 驱动 `cvi_tpu_interface.c`）：`submit` 只把任务
//! 入队并唤醒常驻 worker 线程后立即返回；worker 线程串行调用
//! [`Sg2002Tpu::run_one`] 跑硬件，并按 IRQ evidence generation 阻塞等待 TDMA
//! 完成；`wait` 按 `(tid, seq_no)` 睡 `DONE_WQ`，被 worker 完成时唤醒。
//!
//! SG2002 默认单核，worker 等硬件时必须真正睡眠让出 CPU，相机前处理才能
//! 与 TPU 推理重叠。
//!
//! # 接口约定（重要）
//!
//! - **`submit` 与 `wait` 必须在同一线程调用。** 完成项以 `(提交线程 tid,
//!   用户 seq_no)` 为匹配键存入全局 `DONE_LIST`；`wait` 用「当前线程 tid +
//!   传入 seq_no」检索。换线程 `wait` 会查不到结果而超时。该约束等价于原
//!   Linux 驱动以 `current->pid` 隔离任务的语义，并隔离了不同进程/线程偶然
//!   使用相同 `seq_no` 时的串扰（否则一个 waiter 可能取走他人的完成项）。
//! - **`seq_no` 由用户态提供，仅需在「同一线程的在途请求之间」唯一。** 它不是
//!   内核分配的全局令牌；跨线程不保证唯一也无需唯一，因为 tid 已隔离。
//! - **buffer 生命周期：** `submit` 入队的 [`TpuTask`] 持有底层 Ion buffer 的
//!   `Arc` 强引用，直到结果被 `wait` 取走（或因 `DONE_LIST` 超限被丢弃）。
//!   因此用户在 worker 跑完前 `close(fd)` 不会导致 DMA 物理页被回收
//!   （防 use-after-free）。

use alloc::{boxed::Box, collections::VecDeque, string::String, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering},
    time::Duration,
};

use ax_kspin::SpinNoIrq;
use ax_memory_addr::PhysAddr;
use ax_runtime::{
    hal::irq::IrqReturn,
    maintenance::{
        DeviceMaintenanceHandle, LocalIrqWake, MaintenanceCauses, MaintenanceClosed,
        MaintenanceError, MaintenanceIrqAction, MaintenancePublishResult, MaintenanceRegistrar,
        MaintenanceSession, MaintenanceState, MaintenanceThread, spawn_maintenance_domain,
    },
};
use ax_std::os::arceos::task::WaitQueue;
use sg2002_tpu::{
    ion::IonBuffer,
    tpu::{
        Sg2002Tpu, TdmaIrqEvent,
        error::TpuError,
        types::{
            CVITPU_DMABUF_FLUSH, CVITPU_DMABUF_FLUSH_FD, CVITPU_DMABUF_INVLD,
            CVITPU_DMABUF_INVLD_FD, CVITPU_LOAD_TEE, CVITPU_PIO_MODE, CVITPU_SUBMIT_DMABUF,
            CVITPU_SUBMIT_TEE, CVITPU_UNLOAD_TEE, CVITPU_WAIT_DMABUF, CviCacheOpArg,
            CviSubmitDmaArg, CviWaitDmaArg,
        },
    },
};

use crate::{
    file::{get_file_like, ion::IonBufferFile},
    mm::{UserConstPtr, UserPtr},
    pseudofs::DeviceOps,
};

/// 一个 TPU 推理任务（OS glue 侧）。
struct TpuTask {
    /// 提交线程 id。与 `seq_no` 组成复合匹配键，隔离跨进程/线程的相同 seq_no
    /// （对应原 Linux 驱动 `node->pid = current->pid` 的隔离语义）。
    tid: u64,
    /// 序列号，submit / wait 通过 `(tid, seq_no)` 配对结果。
    seq_no: u32,
    /// DMA buffer 虚拟地址。
    vaddr: usize,
    /// DMA buffer 物理地址。
    paddr: u64,
    /// 持有底层 Ion buffer 的强引用，保证 worker 跑硬件、结果被取走之前，
    /// 即使用户提前 close fd，物理 DMA 页也不会被回收（防 use-after-free）。
    _buffer: Arc<IonBuffer>,
    /// 执行结果（0 成功，-1 失败），由 worker 回填。
    ret: i32,
}

/// 待执行任务队列（对应 Linux `task_list`）。
static TASK_LIST: SpinNoIrq<VecDeque<TpuTask>> = SpinNoIrq::new(VecDeque::new());
/// 已完成任务队列（对应 Linux `done_list`）。
static DONE_LIST: SpinNoIrq<VecDeque<TpuTask>> = SpinNoIrq::new(VecDeque::new());
/// `DONE_LIST` 上限。每个滞留完成项持有一个 `Arc<IonBuffer>`，提交后不 wait
/// 的线程会令其无限累积；超限丢弃最旧项以释放 buffer（对应原驱动
/// `DONE_LIST_MAX`）。
const DONE_LIST_MAX: usize = 64;
/// 唤醒等待结果的提交者（对应 Linux `done_wait_queue`）。
static DONE_WQ: WaitQueue = WaitQueue::new();
/// worker 线程是否已启动（保证只 spawn 一次）。
static WORKER_SPAWNED: AtomicBool = AtomicBool::new(false);
/// The driver wait hook has no context parameter, so the single pinned owner
/// publishes its pinned runtime here for the duration of hardware service.
static TPU_OWNER_PTR: AtomicPtr<TpuOwnerRuntime> = AtomicPtr::new(core::ptr::null_mut());

const TPU_MAINTENANCE_CPU: usize = 0;
const TPU_EVENT_BATCH_LIMIT: usize = 64;
const TPU_START_PENDING: u8 = 0;
const TPU_START_READY: u8 = 1;
const TPU_START_FAILED: u8 = 2;

#[derive(Clone, Copy, Debug)]
enum TpuMaintenanceEvent {
    Tdma(TdmaIrqEvent),
}

struct TpuMaintenanceStartup {
    state: AtomicU8,
    line_quenched: AtomicBool,
    uncontained: AtomicBool,
    wait: WaitQueue,
    remote: SpinNoIrq<Option<DeviceMaintenanceHandle<TpuMaintenanceEvent>>>,
}

impl TpuMaintenanceStartup {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(TPU_START_PENDING),
            line_quenched: AtomicBool::new(false),
            uncontained: AtomicBool::new(false),
            wait: WaitQueue::new(),
            remote: SpinNoIrq::new(None),
        }
    }

    fn publish_ready(&self, remote: DeviceMaintenanceHandle<TpuMaintenanceEvent>) {
        *self.remote.lock() = Some(remote);
        self.state.store(TPU_START_READY, Ordering::Release);
        self.wait.notify_all();
    }

    fn publish_failed(&self) {
        self.state.store(TPU_START_FAILED, Ordering::Release);
        self.wait.notify_all();
    }

    fn take_remote(&self) -> Option<DeviceMaintenanceHandle<TpuMaintenanceEvent>> {
        self.wait
            .wait_until(|| self.state.load(Ordering::Acquire) != TPU_START_PENDING);
        if self.state.load(Ordering::Acquire) != TPU_START_READY {
            return None;
        }
        self.remote.lock().take()
    }
}

struct TpuMaintenanceRuntime {
    remote: DeviceMaintenanceHandle<TpuMaintenanceEvent>,
    _thread: MaintenanceThread,
}

impl TpuMaintenanceRuntime {
    fn is_live(&self) -> bool {
        self.remote.state() == MaintenanceState::Live
    }
}

impl Drop for TpuMaintenanceRuntime {
    fn drop(&mut self) {
        let _ = self.remote.request_shutdown();
    }
}

/// TPU 字符设备
pub struct TpuDevice {
    /// 硬件层
    hw: Arc<Sg2002Tpu>,
    resource: TpuResource,
    /// Fixed-CPU owner for TDMA IRQ and all hardware execution.
    maintenance: Option<TpuMaintenanceRuntime>,
}

const TPU_COMPATIBLES: &[&str] = &["cvitek,tpu"];
const TPU_TDMA_IRQ_NAME: &str = "tdma_irq";
const TPU_DEFAULT_MMIO_SIZE: usize = 0x1000;

/// 等待 TDMA 完成的总超时（约 10 秒）。
const TPU_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
struct TpuResource {
    tdma_paddr: usize,
    tdma_size: usize,
    tiu_paddr: usize,
    tiu_size: usize,
    irq: Option<ax_runtime::hal::irq::IrqId>,
}

impl TpuResource {
    fn probe() -> Option<Self> {
        let resource = Self::from_fdt();
        if resource.is_none() {
            warn!("[TPU] cvitek,tpu node not found or invalid in FDT");
        }
        resource
    }

    fn from_fdt() -> Option<Self> {
        rdrive::with_fdt(|fdt| {
            fdt.find_compatible(TPU_COMPATIBLES)
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
        let tdma = regs.next()?;
        let tiu = regs.next()?;
        let irq = match resolve_named_fdt_irq(&node, TPU_TDMA_IRQ_NAME) {
            Ok(irq) => irq,
            Err(err) => {
                warn!("[TPU] failed to resolve {TPU_TDMA_IRQ_NAME}: {err:?}");
                return None;
            }
        };

        Some(Self {
            tdma_paddr: tdma.address as usize,
            tdma_size: tdma.size.unwrap_or(TPU_DEFAULT_MMIO_SIZE as u64) as usize,
            tiu_paddr: tiu.address as usize,
            tiu_size: tiu.size.unwrap_or(TPU_DEFAULT_MMIO_SIZE as u64) as usize,
            irq,
        })
    }
}

fn resolve_named_fdt_irq(
    node: &rdrive::probe::fdt::NodeType<'_>,
    name: &str,
) -> Result<Option<ax_runtime::hal::irq::IrqId>, ax_runtime::hal::irq::IrqError> {
    let Some(irq) = ax_driver::binding_irq_from_named_fdt_interrupt(node, name)
        .map_err(|_| ax_runtime::hal::irq::IrqError::Unsupported)?
    else {
        return Ok(None);
    };
    ax_runtime::irq::resolve_binding_irq(irq).map(Some)
}

fn map_tpu_mmio(resource: TpuResource) -> Option<(*mut u8, *mut u8)> {
    let tdma = match axklib::mem::iomap(PhysAddr::from(resource.tdma_paddr), resource.tdma_size) {
        Ok(vaddr) => vaddr.as_mut_ptr(),
        Err(err) => {
            warn!(
                "[TPU] failed to map TDMA MMIO at {:#x}+{:#x}: {err:?}",
                resource.tdma_paddr, resource.tdma_size
            );
            return None;
        }
    };
    let tiu = match axklib::mem::iomap(PhysAddr::from(resource.tiu_paddr), resource.tiu_size) {
        Ok(vaddr) => vaddr.as_mut_ptr(),
        Err(err) => {
            warn!(
                "[TPU] failed to map TIU MMIO at {:#x}+{:#x}: {err:?}",
                resource.tiu_paddr, resource.tiu_size
            );
            return None;
        }
    };
    Some((tdma, tiu))
}

/// 注入给 driver core 的阻塞等待函数：在超时内睡眠等待 TDMA 中断到达。
///
/// 由 worker 线程上下文调用（普通可调度任务），睡眠让出 CPU。TDMA IRQ 仅
/// 直接唤醒固定 worker registration；返回 `true` 表示观察到硬件完成，`false`
/// 表示本轮超时或 registration 不变量被破坏。
fn tpu_wait_irq(observed_generation: u64, timeout_us: u64) -> bool {
    let owner = TPU_OWNER_PTR.load(Ordering::Acquire);
    if owner.is_null() {
        return false;
    }
    // SAFETY: only the fixed owner publishes this pointer, after boxing the
    // runtime, and clears it before beginning IRQ teardown or freeing the box.
    // The injected wait hook is called only by that same owner thread.
    unsafe { (&*owner).wait_for_tdma_irq(observed_generation, timeout_us) }
}

struct TpuOwnerRuntime {
    hw: Arc<Sg2002Tpu>,
    startup: Arc<TpuMaintenanceStartup>,
    session: MaintenanceSession<TpuMaintenanceEvent>,
    registration: Option<MaintenanceIrqAction>,
    shutdown_requested: AtomicBool,
    mailbox_fault: AtomicBool,
}

impl TpuOwnerRuntime {
    fn wait_for_tdma_irq(&self, observed_generation: u64, timeout_us: u64) -> bool {
        let deadline = ax_runtime::hal::time::monotonic_time_nanos()
            .saturating_add(timeout_us.saturating_mul(1_000));
        loop {
            if self.drain_pending().is_err() || self.mailbox_fault.swap(false, Ordering::AcqRel) {
                return false;
            }
            if self.hw.irq_generation() != observed_generation {
                return true;
            }
            if ax_runtime::hal::time::monotonic_time_nanos() >= deadline
                || self.session.wait_for_pending_until(deadline).is_err()
            {
                return false;
            }
        }
    }

    fn drain_pending(&self) -> Result<bool, MaintenanceError> {
        let drain = self
            .session
            .drain_owner(TPU_EVENT_BATCH_LIMIT, |event| match event {
                TpuMaintenanceEvent::Tdma(_event) => {}
            })?;
        if drain.causes().contains(MaintenanceCauses::OVERFLOW) {
            self.mailbox_fault.store(true, Ordering::Release);
        }
        if drain.causes().contains(MaintenanceCauses::SHUTDOWN) {
            self.shutdown_requested.store(true, Ordering::Release);
        }
        if self.startup.uncontained.load(Ordering::Acquire) {
            return Err(MaintenanceError::Irq(
                ax_runtime::hal::irq::IrqError::Unsupported,
            ));
        }
        if self.startup.line_quenched.load(Ordering::Acquire) {
            self.registration()?.release_quench()?;
            self.startup.line_quenched.store(false, Ordering::Release);
        }
        Ok(drain.pending())
    }

    fn registration(&self) -> Result<&MaintenanceIrqAction, MaintenanceError> {
        self.registration.as_ref().ok_or(MaintenanceError::Irq(
            ax_runtime::hal::irq::IrqError::NotFound,
        ))
    }
}

fn tpu_irq_action(
    actual_cpu: usize,
    owner_cpu: usize,
    hw: &Sg2002Tpu,
    wake: &LocalIrqWake<TpuMaintenanceEvent>,
    startup: &TpuMaintenanceStartup,
) -> IrqReturn {
    if actual_cpu != owner_cpu {
        startup.uncontained.store(true, Ordering::Release);
        startup.line_quenched.store(true, Ordering::Release);
        return IrqReturn::MaskLineAndWake;
    }
    let Some(event) = hw.capture_irq() else {
        return IrqReturn::Unhandled;
    };
    match wake.publish_from_irq(MaintenanceCauses::IRQ, TpuMaintenanceEvent::Tdma(event)) {
        Ok(MaintenancePublishResult::Published) => IrqReturn::Wake,
        Ok(MaintenancePublishResult::Overflowed) | Err(_) => {
            startup.line_quenched.store(true, Ordering::Release);
            IrqReturn::MaskLineAndWake
        }
    }
}

/// 常驻 worker 线程主循环（对应 Linux `work_thread_main`）。
///
/// 串行取任务、调用 `run_one` 跑硬件、回填结果到 `DONE_LIST` 并唤醒等待者。
/// 单 worker 保证硬件串行访问，无需额外 run 锁。
fn tpu_worker(owner: &TpuOwnerRuntime) -> Result<(), MaintenanceError> {
    info!("[TPU] worker thread started");
    loop {
        let mut pending = owner.drain_pending()?;
        if owner.shutdown_requested.load(Ordering::Acquire) && TASK_LIST.lock().is_empty() {
            return Ok(());
        }

        // Take one request. Remote submitters never enter the hardware driver;
        // they transfer ownership into TASK_LIST and wake this pinned domain.
        let mut task = loop {
            if let Some(task) = TASK_LIST.lock().pop_front() {
                break task;
            }
            if pending {
                crate::task::yield_now();
            } else {
                owner.session.wait_for_pending()?;
            }
            pending = owner.drain_pending()?;
            if owner.shutdown_requested.load(Ordering::Acquire) && TASK_LIST.lock().is_empty() {
                return Ok(());
            }
        };

        // 跑硬件：内部等待 TDMA 完成时经注入的 tpu_wait_irq 睡眠让出 CPU。
        task.ret = owner
            .hw
            .run_one(task.seq_no, task.vaddr, task.paddr)
            .map_or(-1, |_| 0);

        // 入队完成结果并唤醒等待者。若提交线程从不 wait（或 wait 前退出），其
        // 完成项会滞留并攥住 `Arc<IonBuffer>` 永不释放——故对 DONE_LIST 设上限，
        // 超限时丢弃最旧项（连带释放其 buffer 强引用），对应原 Linux 驱动的
        // `cvi_tpu_cleanup_done_list`。
        {
            let mut done = DONE_LIST.lock();
            done.push_back(task);
            while done.len() > DONE_LIST_MAX {
                let dropped = done.pop_front();
                if let Some(t) = dropped {
                    warn!(
                        "[TPU] done list full, dropping orphaned result (tid={}, seq_no={})",
                        t.tid, t.seq_no
                    );
                }
            }
        }
        DONE_WQ.notify_all();
    }
}

fn spawn_tpu_maintenance(
    hw: Arc<Sg2002Tpu>,
    irq: ax_runtime::hal::irq::IrqId,
) -> Option<TpuMaintenanceRuntime> {
    let startup = Arc::new(TpuMaintenanceStartup::new());
    let owner_startup = Arc::clone(&startup);
    let thread = match spawn_maintenance_domain::<TpuMaintenanceEvent, _>(
        TPU_MAINTENANCE_CPU,
        String::from("tpu-maintenance"),
        move |registrar| run_tpu_maintenance(hw, irq, owner_startup, registrar),
    ) {
        Ok(thread) => thread,
        Err(error) => {
            warn!("[TPU] failed to spawn maintenance owner: {error}");
            return None;
        }
    };
    let remote = startup.take_remote()?;
    Some(TpuMaintenanceRuntime {
        remote,
        _thread: thread,
    })
}

fn run_tpu_maintenance(
    hw: Arc<Sg2002Tpu>,
    irq: ax_runtime::hal::irq::IrqId,
    startup: Arc<TpuMaintenanceStartup>,
    registrar: MaintenanceRegistrar<TpuMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    let irq_wake = registrar
        .local_irq_wake()
        .inspect_err(|_| startup.publish_failed())?;
    let remote = registrar.remote_handle();
    let owner_cpu = registrar.owner_cpu();
    let callback_hw = Arc::clone(&hw);
    let callback_startup = Arc::clone(&startup);
    let registration = registrar.register_shared_disabled("sg2002-tdma", irq, move |context| {
        tpu_irq_action(
            context.cpu.0,
            owner_cpu,
            &callback_hw,
            &irq_wake,
            &callback_startup,
        )
    });
    let registration = match registration {
        Ok(registration) => registration,
        Err(error) => {
            warn!("[TPU] failed to register TDMA IRQ {irq:?}: {error:?}");
            startup.publish_failed();
            let session = registrar.activate()?;
            return close_tpu_session(session);
        }
    };
    let session = registrar
        .activate()
        .inspect_err(|_| startup.publish_failed())?;
    let mut owner = Box::new(TpuOwnerRuntime {
        hw,
        startup: Arc::clone(&startup),
        session,
        registration: Some(registration),
        shutdown_requested: AtomicBool::new(false),
        mailbox_fault: AtomicBool::new(false),
    });
    let owner_ptr = core::ptr::from_mut(owner.as_mut());
    if TPU_OWNER_PTR
        .compare_exchange(
            core::ptr::null_mut(),
            owner_ptr,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        startup.publish_failed();
        return close_tpu_owner(*owner);
    }
    owner.hw.set_wait_irq_fn(tpu_wait_irq);
    if let Err(error) = owner.hw.init() {
        warn!("[TPU] initialization failed: {error:?}");
        startup.publish_failed();
        TPU_OWNER_PTR.store(core::ptr::null_mut(), Ordering::Release);
        return close_tpu_owner(*owner);
    }
    if let Err(error) = owner.registration()?.enable() {
        warn!("[TPU] failed to enable TDMA IRQ {irq:?}: {error:?}");
        startup.publish_failed();
        TPU_OWNER_PTR.store(core::ptr::null_mut(), Ordering::Release);
        return close_tpu_owner(*owner);
    }
    startup.publish_ready(remote);
    info!("[TPU] TDMA IRQ {irq:?} owned by maintenance CPU {owner_cpu}");

    let worker_result = tpu_worker(&owner);
    TPU_OWNER_PTR.store(core::ptr::null_mut(), Ordering::Release);
    let close_result = close_tpu_owner(*owner);
    match close_result {
        Ok(closed) => {
            worker_result?;
            Ok(closed)
        }
        Err(error) => Err(error),
    }
}

fn close_tpu_owner(mut owner: TpuOwnerRuntime) -> Result<MaintenanceClosed, MaintenanceError> {
    let begin_close = owner.session.begin_close();
    let registration = owner
        .registration
        .take()
        .expect("live TPU owner retains one IRQ registration");
    let TpuOwnerRuntime {
        hw,
        startup,
        session,
        registration: _,
        shutdown_requested: _,
        mailbox_fault: _,
    } = owner;
    if startup.uncontained.load(Ordering::Acquire) {
        let _retained_owner = (hw, startup, registration);
        session.quarantine_and_park();
    }
    if let Err(error) = registration.disable() {
        warn!("[TPU] failed to disable TDMA IRQ action: {error:?}");
        let _retained_owner = (hw, startup, registration);
        session.quarantine_and_park();
    }
    if startup.line_quenched.load(Ordering::Acquire) {
        if let Err(error) = registration.release_quench() {
            warn!("[TPU] failed to release TDMA line quench: {error:?}");
            let _retained_owner = (hw, startup, registration);
            session.quarantine_and_park();
        }
        startup.line_quenched.store(false, Ordering::Release);
    }
    if let Err(error) = registration.synchronize() {
        warn!("[TPU] failed to synchronize TDMA IRQ action: {error:?}");
        let _retained_owner = (hw, startup, registration);
        session.quarantine_and_park();
    }
    if let Err(failure) = registration.close() {
        let (reason, registration) = failure.into_parts();
        warn!("[TPU] failed to destroy TDMA IRQ action: {reason:?}");
        let _retained_owner = (hw, startup, registration);
        session.quarantine_and_park();
    }
    begin_close?;
    close_tpu_session(session)
}

fn close_tpu_session(
    session: MaintenanceSession<TpuMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    if session.state() == MaintenanceState::Live {
        session.begin_close()?;
    }
    while session.state() == MaintenanceState::Closing {
        let drain = session.drain_owner(TPU_EVENT_BATCH_LIMIT, |_| {})?;
        if !drain.pending() {
            break;
        }
    }
    session.try_begin_draining()?;
    session.finish_close()?;
    session.try_into_closed().map_err(|failure| failure.error())
}

impl TpuDevice {
    pub fn probe() -> Option<Self> {
        let resource = TpuResource::probe()?;
        let hw = {
            let (tdma, tiu) = map_tpu_mmio(resource)?;
            Arc::new(unsafe { Sg2002Tpu::from_vaddr(tdma, tiu) })
        };
        Self::setup(hw, resource)
    }

    /// Starts the fixed-CPU hardware owner. The device is published only after
    /// that thread has registered and enabled its own IRQ action.
    fn setup(hw: Arc<Sg2002Tpu>, resource: TpuResource) -> Option<Self> {
        let Some(irq) = resource.irq else {
            warn!("[TPU] TDMA IRQ not available; refusing polling-only activation");
            return None;
        };
        if WORKER_SPAWNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            warn!("[TPU] duplicate hardware-owner activation rejected");
            return None;
        }
        let maintenance = match spawn_tpu_maintenance(Arc::clone(&hw), irq) {
            Some(maintenance) => maintenance,
            None => {
                WORKER_SPAWNED.store(false, Ordering::Release);
                return None;
            }
        };
        info!(
            "[TPU] resource tdma=[{:#x}, +{:#x}) tiu=[{:#x}, +{:#x}) irq={:?} irq_wait={} \
             source=fdt",
            resource.tdma_paddr,
            resource.tdma_size,
            resource.tiu_paddr,
            resource.tiu_size,
            resource.irq,
            maintenance.is_live(),
        );
        Some(Self {
            hw,
            resource,
            maintenance: Some(maintenance),
        })
    }

    /// 提交 DMA buffer 任务：解析 fd → 入队 → 唤醒 worker → 立即返回。
    fn submit_dmabuf(&self, arg: usize) -> Result<usize, TpuError> {
        // SAFETY: the ioctl record contains only integer fields. Copy it into
        // kernel storage so no user reference survives validation or blocking.
        let submit_arg = unsafe { UserConstPtr::<CviSubmitDmaArg>::from(arg).read_abi() }
            .map_err(|_| TpuError::InvalidDmabuf)?;

        debug!(
            "[TPU] submit dmabuf: fd={}, seq_no={}",
            submit_arg.fd, submit_arg.seq_no
        );
        if !self
            .maintenance
            .as_ref()
            .is_some_and(TpuMaintenanceRuntime::is_live)
        {
            warn!("[TPU] TDMA IRQ {:?} not registered", self.resource.irq);
            return Err(TpuError::NotInitialized);
        }

        // 从文件描述符获取 IonBufferFile
        let fd = submit_arg.fd;
        let file = get_file_like(fd).map_err(|_| {
            error!("[TPU] Failed to get file for fd={}", fd);
            TpuError::InvalidDmabuf
        })?;

        // 尝试转换为 IonBufferFile (使用 downcast_arc)
        let ion_file: Arc<IonBufferFile> = file.downcast_arc::<IonBufferFile>().map_err(|_| {
            error!("[TPU] fd={} is not an IonBufferFile", fd);
            TpuError::InvalidDmabuf
        })?;

        // 获取底层 Ion buffer。clone 一份 Arc 强引用随任务存活，确保 worker
        // 访问 DMA 内存期间（即使用户已 close fd）物理页不被回收。
        let buffer = ion_file.buffer().clone();
        debug!(
            "[TPU] dmabuf info: handle={}, size={}, paddr=0x{:x}",
            buffer.handle.as_u32(),
            buffer.size,
            buffer.dma_info.bus_addr.as_u64()
        );

        let task = TpuTask {
            tid: crate::task::current_user_task().id().as_u64(),
            seq_no: submit_arg.seq_no,
            vaddr: buffer.dma_info.cpu_addr.as_ptr() as usize,
            paddr: buffer.dma_info.bus_addr.as_u64(),
            _buffer: buffer,
            ret: 0,
        };

        // 入队并唤醒 worker，随后立即返回（submit 不等推理）。
        let mut tasks = TASK_LIST.lock();
        tasks.push_back(task);
        let published = self
            .maintenance
            .as_ref()
            .expect("live TPU device retains its maintenance owner")
            .remote
            .publish_cause(MaintenanceCauses::SUBMIT);
        if published.is_err() {
            let _rejected = tasks.pop_back();
            return Err(TpuError::NotInitialized);
        }
        drop(tasks);

        Ok(0)
    }

    /// 等待 DMA buffer 完成：按 `(tid, seq_no)` 睡 `DONE_WQ`，被 worker 唤醒后
    /// 取结果。用调用线程 tid 与用户 seq_no 组成复合键，隔离跨进程/线程的相同
    /// seq_no——否则两个进程都从 seq 0 开始会互相取走对方的完成项。
    fn wait_dmabuf(&self, arg: usize) -> Result<usize, TpuError> {
        // SAFETY: the ioctl record contains only integer fields. In particular,
        // do not retain a mutable user reference while the wait queue sleeps.
        let wait_arg = unsafe { UserConstPtr::<CviWaitDmaArg>::from(arg).read_abi() }
            .map_err(|_| TpuError::InvalidDmabuf)?;
        let seq_no = wait_arg.seq_no;
        let tid = crate::task::current_user_task().id().as_u64();

        // 睡在 DONE_WQ 上直到对应 (tid, seq_no) 出现在完成队列（或超时）。
        // wait_timeout_until 睡前复检谓词，等价 Linux wait_event。
        let timed_out = DONE_WQ.wait_timeout_until(TPU_WAIT_TIMEOUT, || {
            DONE_LIST
                .lock()
                .iter()
                .any(|t| t.tid == tid && t.seq_no == seq_no)
        });

        // 取出该任务结果（即使超时也再查一次，处理临界完成）。
        let found = {
            let mut done = DONE_LIST.lock();
            done.iter()
                .position(|t| t.tid == tid && t.seq_no == seq_no)
                .map(|idx| done.remove(idx).unwrap())
        };

        let (ret, result) = match found {
            Some(task) => {
                if task.ret != 0 {
                    (task.ret, Err(TpuError::Timeout))
                } else {
                    (task.ret, Ok(0))
                }
            }
            None => {
                warn!(
                    "[TPU] wait dmabuf: (tid={}, seq_no={}) not found (timed_out={})",
                    tid, seq_no, timed_out
                );
                (-1, Err(TpuError::Timeout))
            }
        };
        UserPtr::<i32>::from(arg + core::mem::offset_of!(CviWaitDmaArg, ret))
            .write(ret)
            .map_err(|_| TpuError::InvalidDmabuf)?;
        result
    }

    /// 刷新 DMA buffer 缓存 (通过物理地址)
    fn cache_flush(&self, arg: usize) -> Result<usize, TpuError> {
        // SAFETY: the ioctl record contains only integer fields.
        let flush_arg = unsafe { UserConstPtr::<CviCacheOpArg>::from(arg).read_abi() }
            .map_err(|_| TpuError::InvalidDmabuf)?;
        self.hw.cache_flush_paddr(flush_arg.paddr, flush_arg.size)?;
        Ok(0)
    }

    /// 无效化 DMA buffer 缓存 (通过物理地址)
    fn cache_invalidate(&self, arg: usize) -> Result<usize, TpuError> {
        // SAFETY: the ioctl record contains only integer fields.
        let invalidate_arg = unsafe { UserConstPtr::<CviCacheOpArg>::from(arg).read_abi() }
            .map_err(|_| TpuError::InvalidDmabuf)?;
        self.hw
            .cache_invalidate_paddr(invalidate_arg.paddr, invalidate_arg.size)?;
        Ok(0)
    }

    /// 刷新 DMA buffer 缓存 (通过 fd)
    fn dmabuf_flush_fd(&self, arg: usize) -> Result<usize, TpuError> {
        let fd = arg as i32;
        debug!("TPU dmabuf flush fd: {}", fd);
        let buffer = self.lookup_ion_buffer(fd)?;
        let paddr = buffer.dma_info.bus_addr.as_u64();
        let size = buffer.size as u64;
        self.hw.cache_flush_paddr(paddr, size)?;
        debug!("Flushed buffer: paddr=0x{:x}, size={}", paddr, size);
        Ok(0)
    }

    /// 无效化 DMA buffer 缓存 (通过 fd)
    fn dmabuf_invld_fd(&self, arg: usize) -> Result<usize, TpuError> {
        let fd = arg as i32;
        debug!("TPU dmabuf invalidate fd: {}", fd);
        let buffer = self.lookup_ion_buffer(fd)?;
        let paddr = buffer.dma_info.bus_addr.as_u64();
        let size = buffer.size as u64;
        self.hw.cache_invalidate_paddr(paddr, size)?;
        Ok(0)
    }

    /// 把用户传入的 fd 解析为底层 [`sg2002_tpu::ion::IonBuffer`]。
    ///
    /// fd（由 `add_file_like` 分配的文件描述符）与 Ion 内部 handle（来自
    /// `IonHandle` 的全局递增计数）属于两个独立的编号空间，不能直接互相替代。
    /// 因此这里走和 `submit_dmabuf` 一致的路径：fd → `IonBufferFile` →
    /// 持有的 `Arc<IonBuffer>`。
    fn lookup_ion_buffer(&self, fd: i32) -> Result<Arc<IonBuffer>, TpuError> {
        let file = get_file_like(fd).map_err(|err| {
            error!("[TPU] failed to get file for fd={}: {:?}", fd, err);
            TpuError::InvalidDmabuf
        })?;
        let ion_file: Arc<IonBufferFile> = file.downcast_arc::<IonBufferFile>().map_err(|_| {
            error!("[TPU] fd={} is not an IonBufferFile", fd);
            TpuError::InvalidDmabuf
        })?;
        Ok(ion_file.buffer().clone())
    }
}

impl DeviceOps for TpuDevice {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> axfs_ng_vfs::VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> axfs_ng_vfs::VfsResult<usize> {
        Ok(0)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> axfs_ng_vfs::VfsResult<usize> {
        debug!("TPU ioctl: cmd=0x{:x}, arg=0x{:x}", cmd, arg);

        let result = match cmd {
            CVITPU_SUBMIT_DMABUF => self.submit_dmabuf(arg),
            CVITPU_DMABUF_FLUSH_FD => self.dmabuf_flush_fd(arg),
            CVITPU_DMABUF_INVLD_FD => self.dmabuf_invld_fd(arg),
            CVITPU_DMABUF_FLUSH => self.cache_flush(arg),
            CVITPU_DMABUF_INVLD => self.cache_invalidate(arg),
            CVITPU_WAIT_DMABUF => self.wait_dmabuf(arg),
            CVITPU_PIO_MODE => {
                warn!("TPU PIO mode not implemented");
                Ok(0)
            }
            CVITPU_LOAD_TEE | CVITPU_SUBMIT_TEE | CVITPU_UNLOAD_TEE => {
                warn!("TPU TEE operations not supported");
                Err(TpuError::NotInitialized)
            }
            _ => {
                warn!("Unknown TPU ioctl command: 0x{:x}", cmd);
                Err(TpuError::NotInitialized)
            }
        };

        match result {
            Ok(v) => Ok(v),
            Err(e) => {
                error!("TPU ioctl error: {:?}", e);
                Err(ax_errno::AxError::Unsupported)
            }
        }
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
