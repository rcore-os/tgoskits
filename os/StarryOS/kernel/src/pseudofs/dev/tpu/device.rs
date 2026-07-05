//! TPU 设备 OS 适配
//!
//! 将 ioctl 命令翻译为 `Sg2002Tpu` 调用，并通过 fd 解析 Ion buffer
//! 物理/虚拟地址。
//!
//! 异步模型（复刻原 Linux 驱动 `cvi_tpu_interface.c`）：`submit` 只把任务
//! 入队并唤醒常驻 worker 线程后立即返回；worker 线程串行调用
//! [`Sg2002Tpu::run_one`] 跑硬件，等待 TDMA 完成时通过 `IRQ_WQ` 睡眠让出
//! CPU；`wait` 按 `(tid, seq_no)` 睡 `DONE_WQ`，被 worker 完成时唤醒。
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

use alloc::{collections::VecDeque, string::String, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
    time::Duration,
};

use ax_kspin::SpinNoIrq;
use ax_memory_addr::PhysAddr;
use ax_task::WaitQueue;
use sg2002_tpu::{
    ion::IonBuffer,
    tpu::{
        Sg2002Tpu,
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
    pseudofs::{
        DeviceOps,
        dev::{IrqRegistration, request_shared_disabled},
    },
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
/// 唤醒 worker 取任务（对应 Linux `task_wait_queue`）。
static TASK_WQ: WaitQueue = WaitQueue::new();
/// 唤醒等待结果的提交者（对应 Linux `done_wait_queue`）。
static DONE_WQ: WaitQueue = WaitQueue::new();
/// TDMA 硬件中断到达时唤醒在此睡眠的 worker。
static IRQ_WQ: WaitQueue = WaitQueue::new();
/// worker 线程是否已启动（保证只 spawn 一次）。
static WORKER_SPAWNED: AtomicBool = AtomicBool::new(false);
/// 指向唯一 TPU 硬件实例，供注入的 [`tpu_wait_irq`] 读取中断标志。
///
/// SG2002 只有一个 TPU；`Sg2002Tpu` 由 worker 持有的 `Arc` 保活，实际生命
/// 周期与内核同长，这里的裸指针始终有效。
static HW_PTR: AtomicPtr<Sg2002Tpu> = AtomicPtr::new(core::ptr::null_mut());

/// TPU 字符设备
pub struct TpuDevice {
    /// 硬件层
    hw: Arc<Sg2002Tpu>,
    resource: TpuResource,
    /// TDMA IRQ action registration.
    irq_registration: Option<IrqRegistration>,
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

fn register_tpu_irq(
    irq: Option<ax_runtime::hal::irq::IrqId>,
    hw: &Arc<Sg2002Tpu>,
) -> Option<IrqRegistration> {
    let Some(irq) = irq else {
        warn!("[TPU] TDMA IRQ not available; execution will use MMIO poll fallback");
        return None;
    };
    let hw = Arc::clone(hw);
    let registration = match request_shared_disabled(irq, move |_| {
        if hw.handle_irq() {
            warn!("[TPU] TDMA IRQ {irq:?} reports error status");
        }
        // 唤醒在 IRQ_WQ 上睡眠的 worker。中断上下文不重调度（resched=false），
        // 对齐 kpu.rs 的做法；WaitQueue 由 SpinNoIrq 守护，IRQ 内 notify 安全。
        IRQ_WQ.notify_all(false);
        ax_runtime::hal::irq::IrqReturn::Handled
    }) {
        Ok(registration) => registration,
        Err(err) => {
            warn!("[TPU] failed to register TDMA IRQ {irq:?}: {err:?}");
            return None;
        }
    };
    if let Err(err) = registration.enable() {
        warn!("[TPU] failed to enable TDMA IRQ {irq:?}: {err:?}");
        return None;
    }
    info!("[TPU] TDMA IRQ {irq:?} registered and enabled");
    Some(registration)
}

/// 注入给 driver core 的阻塞等待函数：在超时内睡眠等待 TDMA 中断到达。
///
/// 由 worker 线程上下文调用（普通可调度任务），睡眠让出 CPU；硬件中断到达时
/// `tpu_tdma_irq_handler` 经 `IRQ_WQ` 唤醒。返回 `true` 表示中断已到达，
/// `false` 表示本轮超时。
fn tpu_wait_irq(timeout_us: u64) -> bool {
    let hw = HW_PTR.load(Ordering::Acquire);
    if hw.is_null() {
        return false;
    }
    // SAFETY: HW_PTR 指向 worker 持有的 Arc 内的实例，生命周期与内核同长。
    let hw = unsafe { &*hw };
    // wait_timeout_until 在睡前于队列锁内复检谓词，等价 Linux wait_event，
    // 无唤醒先于等待的丢失风险。返回 true 表示超时。
    !IRQ_WQ.wait_timeout_until(Duration::from_micros(timeout_us), || hw.irq_pending())
}

/// 常驻 worker 线程主循环（对应 Linux `work_thread_main`）。
///
/// 串行取任务、调用 `run_one` 跑硬件、回填结果到 `DONE_LIST` 并唤醒等待者。
/// 单 worker 保证硬件串行访问，无需额外 run 锁。
fn tpu_worker(hw: Arc<Sg2002Tpu>) {
    info!("[TPU] worker thread started");
    loop {
        // 取一个任务；队列空则睡在 TASK_WQ 上让出 CPU。
        // 注意：拿到 guard 后立即在表达式内释放，绝不持锁调用 wait*。
        let mut task = loop {
            if let Some(task) = TASK_LIST.lock().pop_front() {
                break task;
            }
            TASK_WQ.wait_until(|| !TASK_LIST.lock().is_empty());
        };

        // 跑硬件：内部等待 TDMA 完成时经注入的 tpu_wait_irq 睡眠让出 CPU。
        task.ret = hw
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
        DONE_WQ.notify_all(false);
    }
}

impl TpuDevice {
    pub fn probe() -> Option<Self> {
        let resource = TpuResource::probe()?;
        let hw = {
            let (tdma, tiu) = map_tpu_mmio(resource)?;
            Arc::new(unsafe { Sg2002Tpu::from_vaddr(tdma, tiu) })
        };
        Some(Self::setup(hw, resource))
    }

    /// 公共初始化：注入等待函数、注册中断、启动 worker 线程。
    fn setup(hw: Arc<Sg2002Tpu>, resource: TpuResource) -> Self {
        hw.set_wait_irq_fn(tpu_wait_irq);
        if let Err(err) = hw.init() {
            warn!("[TPU] init failed: {:?}", err);
        }
        let irq_registration = register_tpu_irq(resource.irq, &hw);
        info!(
            "[TPU] resource tdma=[{:#x}, +{:#x}) tiu=[{:#x}, +{:#x}) irq={:?} irq_wait={} \
             source=fdt",
            resource.tdma_paddr,
            resource.tdma_size,
            resource.tiu_paddr,
            resource.tiu_size,
            resource.irq,
            irq_registration.is_some(),
        );

        // 发布硬件指针供 tpu_wait_irq 读取中断标志，并启动唯一 worker 线程。
        if WORKER_SPAWNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            HW_PTR.store(Arc::as_ptr(&hw) as *mut Sg2002Tpu, Ordering::Release);
            let worker_hw = hw.clone();
            ax_task::spawn_with_name(move || tpu_worker(worker_hw), String::from("tpu-worker"));
        }

        Self {
            hw,
            resource,
            irq_registration,
        }
    }

    /// 提交 DMA buffer 任务：解析 fd → 入队 → 唤醒 worker → 立即返回。
    fn submit_dmabuf(&self, arg: usize) -> Result<usize, TpuError> {
        // 从用户空间读取参数
        let submit_arg = unsafe { &*(arg as *const CviSubmitDmaArg) };

        debug!(
            "[TPU] submit dmabuf: fd={}, seq_no={}",
            submit_arg.fd, submit_arg.seq_no
        );
        if self.irq_registration.is_none() {
            warn!("[TPU] TDMA IRQ {:?} not registered", self.resource.irq);
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
            tid: ax_task::current().id().as_u64(),
            seq_no: submit_arg.seq_no,
            vaddr: buffer.dma_info.cpu_addr.as_ptr() as usize,
            paddr: buffer.dma_info.bus_addr.as_u64(),
            _buffer: buffer,
            ret: 0,
        };

        // 入队并唤醒 worker，随后立即返回（submit 不等推理）。
        TASK_LIST.lock().push_back(task);
        TASK_WQ.notify_one(true);

        Ok(0)
    }

    /// 等待 DMA buffer 完成：按 `(tid, seq_no)` 睡 `DONE_WQ`，被 worker 唤醒后
    /// 取结果。用调用线程 tid 与用户 seq_no 组成复合键，隔离跨进程/线程的相同
    /// seq_no——否则两个进程都从 seq 0 开始会互相取走对方的完成项。
    fn wait_dmabuf(&self, arg: usize) -> Result<usize, TpuError> {
        let wait_arg = unsafe { &mut *(arg as *mut CviWaitDmaArg) };
        let seq_no = wait_arg.seq_no;
        let tid = ax_task::current().id().as_u64();

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

        match found {
            Some(task) => {
                wait_arg.ret = task.ret;
                if task.ret != 0 {
                    return Err(TpuError::Timeout);
                }
                Ok(0)
            }
            None => {
                wait_arg.ret = -1;
                warn!(
                    "[TPU] wait dmabuf: (tid={}, seq_no={}) not found (timed_out={})",
                    tid, seq_no, timed_out
                );
                Err(TpuError::Timeout)
            }
        }
    }

    /// 刷新 DMA buffer 缓存 (通过物理地址)
    fn cache_flush(&self, arg: usize) -> Result<usize, TpuError> {
        let flush_arg = unsafe { &*(arg as *const CviCacheOpArg) };
        self.hw.cache_flush_paddr(flush_arg.paddr, flush_arg.size)?;
        Ok(0)
    }

    /// 无效化 DMA buffer 缓存 (通过物理地址)
    fn cache_invalidate(&self, arg: usize) -> Result<usize, TpuError> {
        let invalidate_arg = unsafe { &*(arg as *const CviCacheOpArg) };
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
