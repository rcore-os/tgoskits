//! TPU 设备 OS 适配
//!
//! 将 ioctl 命令翻译为 `Sg2002Tpu` 调用，并通过 fd 解析 Ion buffer
//! 物理/虚拟地址。
//!
//! 异步模型（复刻原 Linux 驱动 `cvi_tpu_interface.c`）：`submit` 只把任务
//! 入队并唤醒常驻 worker 线程后立即返回；worker 线程串行调用
//! [`Sg2002Tpu::run_one`] 跑硬件，等待 TDMA 完成时通过 `IRQ_WQ` 睡眠让出
//! CPU；`wait` 按 `seq_no` 睡 `DONE_WQ`，被 worker 完成时唤醒。
//!
//! SG2002 默认单核，worker 等硬件时必须真正睡眠让出 CPU，相机前处理才能
//! 与 TPU 推理重叠。

use alloc::{collections::VecDeque, string::String, sync::Arc};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering},
    time::Duration,
};

use ax_kspin::SpinNoIrq;
use ax_runtime::hal::time::monotonic_time_nanos;
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
    pseudofs::DeviceOps,
};

/// 一个 TPU 推理任务（OS glue 侧）。
struct TpuTask {
    /// 序列号，submit / wait 通过它配对结果。
    seq_no: u32,
    /// DMA buffer 虚拟地址。
    vaddr: usize,
    /// DMA buffer 物理地址。
    paddr: u64,
    /// 执行结果（0 成功，-1 失败），由 worker 回填。
    ret: i32,
    /// submit 入队时刻（ns），用于度量 submit→完成 的端到端延迟。
    submit_ns: u64,
}

/// 待执行任务队列（对应 Linux `task_list`）。
static TASK_LIST: SpinNoIrq<VecDeque<TpuTask>> = SpinNoIrq::new(VecDeque::new());
/// 已完成任务队列（对应 Linux `done_list`）。
static DONE_LIST: SpinNoIrq<VecDeque<TpuTask>> = SpinNoIrq::new(VecDeque::new());
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

// ---- 一帧期间「等硬件」的计量（单 worker 串行，无并发竞争）----
/// 本帧 `run_one` 内 `tpu_wait_irq` 累计睡眠让出的纳秒数。
static WAIT_SLEEP_NS: AtomicU64 = AtomicU64::new(0);
/// 本帧 `run_one` 内 `tpu_wait_irq` 被调用的次数（= fire→等中断 的段数）。
static WAIT_CALLS: AtomicU64 = AtomicU64::new(0);

/// TPU 字符设备
pub struct TpuDevice {
    /// 硬件层
    hw: Arc<Sg2002Tpu>,
    /// 是否已注册中断
    irq_registered: bool,
}

// From SG2002 TPU DT node:
// interrupts = <0x4b 0x04 0x4c 0x04>;
// interrupt-names = "tiu_irq\0tdma_irq";
// so TDMA uses the second IRQ: 0x4c (76).
const TPU_TDMA_IRQ: usize = 76;

/// 等待 TDMA 完成的总超时（约 10 秒）。
const TPU_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

fn register_tpu_irq(hw: &Arc<Sg2002Tpu>) -> bool {
    let data = unsafe { NonNull::new_unchecked(Arc::as_ptr(hw) as *mut ()) };
    if ax_runtime::hal::irq::request_shared_irq(TPU_TDMA_IRQ, tpu_tdma_irq_handler, data).is_err() {
        warn!("[TPU] failed to register tdma irq {}", TPU_TDMA_IRQ);
        return false;
    }
    ax_runtime::hal::irq::set_enable(TPU_TDMA_IRQ, true);
    true
}

unsafe fn tpu_tdma_irq_handler(
    _ctx: ax_runtime::hal::irq::IrqContext,
    data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    let hw = unsafe { &*(data.as_ptr() as *const Sg2002Tpu) };
    if hw.handle_irq() {
        warn!("[TPU] tdma irq {} reports error status", TPU_TDMA_IRQ);
    }
    // 唤醒在 IRQ_WQ 上睡眠的 worker。中断上下文不重调度（resched=false），
    // 对齐 kpu.rs 的做法；WaitQueue 由 SpinNoIrq 守护，IRQ 内 notify 安全。
    IRQ_WQ.notify_all(false);
    ax_runtime::hal::irq::IrqReturn::Handled
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

    // 度量这一轮真正睡眠让出的时长：进入前后各取一次 monotonic 时钟。
    // 这段 delta 就是 worker 把 CPU 让出去的窗口，sched_probe 会记录这期间
    // CPU 跑了谁（idle 还是其他任务）。
    let enter = monotonic_time_nanos();
    // wait_timeout_until 在睡前于队列锁内复检谓词，等价 Linux wait_event，
    // 无唤醒先于等待的丢失风险。返回 true 表示超时。
    let timed_out =
        IRQ_WQ.wait_timeout_until(Duration::from_micros(timeout_us), || hw.irq_pending());
    let slept = monotonic_time_nanos().saturating_sub(enter);

    WAIT_SLEEP_NS.fetch_add(slept, Ordering::Relaxed);
    WAIT_CALLS.fetch_add(1, Ordering::Relaxed);

    !timed_out
}

/// 常驻 worker 线程主循环（对应 Linux `work_thread_main`）。
///
/// 串行取任务、调用 `run_one` 跑硬件、回填结果到 `DONE_LIST` 并唤醒等待者。
/// 单 worker 保证硬件串行访问，无需额外 run 锁。
///
/// 每跑完一帧打印量化指标（见 [`log_frame_metrics`]）。
fn tpu_worker(hw: Arc<Sg2002Tpu>) {
    info!("[TPU] worker thread started");
    // 登记 worker 自身 tid 与 idle tid，供 sched_probe 区分「让给 idle(空转)」
    // 与「让给其他任务(有效重叠)」。SG2002 单核，idle 任务即本 CPU 的 idle。
    super::sched_probe::set_worker_tid(ax_task::current().id().as_u64());
    super::sched_probe::set_idle_tid(ax_task::current_idle_task_id().as_u64());
    loop {
        // 取一个任务；队列空则睡在 TASK_WQ 上让出 CPU。
        // 注意：拿到 guard 后立即在表达式内释放，绝不持锁调用 wait*。
        let mut task = loop {
            if let Some(task) = TASK_LIST.lock().pop_front() {
                break task;
            }
            TASK_WQ.wait_until(|| !TASK_LIST.lock().is_empty());
        };

        // ---- 帧级量化：清零本帧等硬件计量，取调度快照基线 ----
        WAIT_SLEEP_NS.store(0, Ordering::Relaxed);
        WAIT_CALLS.store(0, Ordering::Relaxed);
        super::sched_probe::reset_targets();
        let sched_base = super::sched_probe::snapshot();
        let (irq_before, poll_before) = hw.irq_stats();
        let run_start = monotonic_time_nanos();

        // 跑硬件：内部等待 TDMA 完成时经注入的 tpu_wait_irq 睡眠让出 CPU。
        task.ret = hw
            .run_one(task.seq_no, task.vaddr, task.paddr)
            .map_or(-1, |_| 0);

        // ---- 帧级量化：结算并打印 ----
        let run_ns = monotonic_time_nanos().saturating_sub(run_start);
        let sleep_ns = WAIT_SLEEP_NS.load(Ordering::Relaxed);
        let wait_calls = WAIT_CALLS.load(Ordering::Relaxed);
        let (irq_after, poll_after) = hw.irq_stats();
        let sched = super::sched_probe::snapshot().delta_since(&sched_base);
        log_frame_metrics(
            task.seq_no,
            task.submit_ns,
            run_start,
            run_ns,
            sleep_ns,
            wait_calls,
            irq_after.saturating_sub(irq_before),
            poll_after.saturating_sub(poll_before),
            &sched,
        );
        log_yield_targets(task.seq_no);

        DONE_LIST.lock().push_back(task);
        DONE_WQ.notify_all(false);
    }
}

/// 打印一帧的量化指标，回答用户三个问题：
/// 1. 这一帧 CPU 真正在算 vs 空转/让出了多少；
/// 2. worker 等硬件期间让出的 CPU 去跑了什么（idle=空转 / other=有效重叠）；
/// 3. 走的是真 IRQ 还是 MMIO 轮询兜底。
#[allow(clippy::too_many_arguments)]
fn log_frame_metrics(
    seq_no: u32,
    submit_ns: u64,
    run_start_ns: u64,
    run_ns: u64,
    sleep_ns: u64,
    wait_calls: u64,
    irq_delta: u64,
    poll_delta: u64,
    sched: &super::sched_probe::SchedSnapshot,
) {
    // run_ns = busy_ns(真正占 CPU 跑硬件编程/poll) + sleep_ns(让出等中断)
    let busy_ns = run_ns.saturating_sub(sleep_ns);
    // 入队到 worker 开跑的排队延迟（submit→run_one 起点）。
    let queue_ns = run_start_ns.saturating_sub(submit_ns);
    // 端到端：submit 到这一帧硬件跑完。
    let e2e_ns = run_start_ns
        .saturating_add(run_ns)
        .saturating_sub(submit_ns);

    let pct =
        |part: u64, whole: u64| -> u64 { part.saturating_mul(100).checked_div(whole).unwrap_or(0) };

    info!(
        "[TPU][metrics] seq={seq} | e2e={e2e}us queue={queue}us run={run}us | \
         busy={busy}us({busy_pct}%) sleep={sleep}us({sleep_pct}%) wait_calls={wc} | yield→ \
         idle={idle}us other={other}us sys_switches={sw} worker_switches={wsw} | irq={irq} \
         poll_fallback={poll}",
        seq = seq_no,
        e2e = e2e_ns / 1000,
        queue = queue_ns / 1000,
        run = run_ns / 1000,
        busy = busy_ns / 1000,
        busy_pct = pct(busy_ns, run_ns),
        sleep = sleep_ns / 1000,
        sleep_pct = pct(sleep_ns, run_ns),
        wc = wait_calls,
        idle = sched.idle_ns / 1000,
        other = sched.other_ns / 1000,
        sw = sched.switch_count,
        wsw = sched.worker_switch_count,
        irq = irq_delta,
        poll = poll_delta,
    );
}

/// 打印这一帧 worker 让出 CPU 时切到了哪些任务、各多少次（按次数降序）。
/// 任务名由调度切换点直接捎带缓存（含内核线程，如 idle/gc）；哨兵 `tid==0`
/// 表示槽溢出。
fn log_yield_targets(seq_no: u32) {
    let targets = super::sched_probe::targets_summary();
    if targets.is_empty() {
        return;
    }

    let mut line = String::new();
    for t in targets {
        if !line.is_empty() {
            line.push_str(", ");
        }
        if t.tid == 0 {
            // 溢出哨兵。
            let _ = core::fmt::write(&mut line, format_args!("<overflow>x{}", t.count));
            continue;
        }
        let name = if t.name.is_empty() { "?" } else { &t.name };
        let _ = core::fmt::write(&mut line, format_args!("{name}(tid{})x{}", t.tid, t.count));
    }

    info!("[TPU][metrics] seq={seq_no} | yield_targets: {line}");
}

impl TpuDevice {
    /// 创建 TPU 设备（使用默认物理地址）
    ///
    /// # Safety
    /// 调用者必须确保偏移计算后的虚拟地址有效。
    pub unsafe fn new() -> Self {
        let hw = Arc::new(unsafe { Sg2002Tpu::new() });
        Self::setup(hw)
    }

    /// 使用指定的虚拟地址创建 TPU 设备
    ///
    /// # Safety
    /// 调用者必须确保虚拟地址有效。
    #[allow(dead_code)]
    pub unsafe fn from_vaddr(tdma_vaddr: *mut u8, tiu_vaddr: *mut u8) -> Self {
        let hw = Arc::new(unsafe { Sg2002Tpu::from_vaddr(tdma_vaddr, tiu_vaddr) });
        Self::setup(hw)
    }

    /// 公共初始化：注入等待函数、注册中断、启动 worker 线程。
    fn setup(hw: Arc<Sg2002Tpu>) -> Self {
        hw.set_wait_irq_fn(tpu_wait_irq);
        if let Err(err) = hw.init() {
            warn!("[TPU] init failed: {:?}", err);
        }
        let irq_registered = register_tpu_irq(&hw);

        // 发布硬件指针供 tpu_wait_irq 读取中断标志，并启动唯一 worker 线程。
        if WORKER_SPAWNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            HW_PTR.store(Arc::as_ptr(&hw) as *mut Sg2002Tpu, Ordering::Release);
            let worker_hw = hw.clone();
            ax_task::spawn_with_name(move || tpu_worker(worker_hw), String::from("tpu-worker"));
        }

        Self { hw, irq_registered }
    }

    /// 提交 DMA buffer 任务：解析 fd → 入队 → 唤醒 worker → 立即返回。
    fn submit_dmabuf(&self, arg: usize) -> Result<usize, TpuError> {
        // 从用户空间读取参数
        let submit_arg = unsafe { &*(arg as *const CviSubmitDmaArg) };

        debug!(
            "[TPU] submit dmabuf: fd={}, seq_no={}",
            submit_arg.fd, submit_arg.seq_no
        );
        if !self.irq_registered {
            warn!(
                "[TPU] tdma irq {} not registered, execution may timeout",
                TPU_TDMA_IRQ
            );
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

        // 获取底层 Ion buffer（fd 持有强引用，保证生命周期）
        let buffer = ion_file.buffer();
        debug!(
            "[TPU] dmabuf info: handle={}, size={}, paddr=0x{:x}",
            buffer.handle.as_u32(),
            buffer.size,
            buffer.dma_info.bus_addr.as_u64()
        );

        let task = TpuTask {
            seq_no: submit_arg.seq_no,
            vaddr: buffer.dma_info.cpu_addr.as_ptr() as usize,
            paddr: buffer.dma_info.bus_addr.as_u64(),
            ret: 0,
            submit_ns: monotonic_time_nanos(),
        };

        // 入队并唤醒 worker，随后立即返回（submit 不等推理）。
        TASK_LIST.lock().push_back(task);
        TASK_WQ.notify_one(true);

        Ok(0)
    }

    /// 等待 DMA buffer 完成：按 `seq_no` 睡 `DONE_WQ`，被 worker 唤醒后取结果。
    fn wait_dmabuf(&self, arg: usize) -> Result<usize, TpuError> {
        let wait_arg = unsafe { &mut *(arg as *mut CviWaitDmaArg) };
        let seq_no = wait_arg.seq_no;

        // 睡在 DONE_WQ 上直到对应 seq_no 出现在完成队列（或超时）。
        // wait_timeout_until 睡前复检谓词，等价 Linux wait_event。
        let timed_out = DONE_WQ.wait_timeout_until(TPU_WAIT_TIMEOUT, || {
            DONE_LIST.lock().iter().any(|t| t.seq_no == seq_no)
        });

        // 取出该任务结果（即使超时也再查一次，处理临界完成）。
        let found = {
            let mut done = DONE_LIST.lock();
            done.iter()
                .position(|t| t.seq_no == seq_no)
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
                    "[TPU] wait dmabuf: seq_no {} not found (timed_out={})",
                    seq_no, timed_out
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
