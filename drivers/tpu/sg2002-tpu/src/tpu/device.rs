//! TPU 设备抽象（硬件层）
//!
//! 提供与 OS 解耦的高层 API，调用方负责将 fd / ioctl 解析为
//! `(seq_no, vaddr, paddr)` 后再调用本模块。

use alloc::collections::VecDeque;
use core::{
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use ax_kspin::SpinNoPreempt as Mutex;

use super::{
    TDMA_PHYS_BASE, TIU_PHYS_BASE,
    error::TpuError,
    platform::{DelayFn, TiuIrqCallback, TpuRuntimeState},
    tdma::TdmaRegs,
    tiu::TiuRegs,
};

/// TPU 设备状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpuState {
    /// 未初始化
    Uninitialized,
    /// 空闲
    Idle,
    /// 运行中
    Running,
    /// 已挂起
    Suspended,
}

/// TPU 任务提交路径
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpuSubmitPath {
    /// 普通描述符模式
    DesNormal = 0,
}

/// TPU 任务节点
#[derive(Debug)]
pub struct TpuTaskNode {
    /// 进程 ID
    pub pid: u32,
    /// 序列号
    pub seq_no: u32,
    /// DMA buffer 文件描述符
    pub dmabuf_fd: i32,
    /// DMA buffer 虚拟地址
    pub dmabuf_vaddr: usize,
    /// DMA buffer 物理地址
    pub dmabuf_paddr: u64,
    /// 提交路径
    pub tpu_path: TpuSubmitPath,
    /// 执行结果
    pub ret: i32,
}

/// TPU 内核工作状态
#[derive(Default)]
pub struct TpuKernelWork {
    /// 任务队列
    pub task_list: VecDeque<TpuTaskNode>,
    /// 完成队列
    pub done_list: VecDeque<TpuTaskNode>,
}

/// TPU 设备内部状态
struct TpuDeviceInner {
    /// TDMA 寄存器
    tdma: TdmaRegs,
    /// TIU 寄存器
    tiu: TiuRegs,
    /// 设备状态
    state: TpuState,
    /// 运行时状态
    runtime: TpuRuntimeState,
    /// 任务工作队列
    kernel_work: TpuKernelWork,
    /// TIU 中断回调
    tiu_irq_callback: Option<TiuIrqCallback>,
}

/// SG2002 TPU 设备（仅硬件层）
pub struct Sg2002Tpu {
    /// TDMA 寄存器基地址
    tdma_vaddr: *mut u8,
    /// TIU 寄存器基地址
    tiu_vaddr: *mut u8,
    /// 内部状态 (使用自旋锁保护)
    inner: Mutex<TpuDeviceInner>,
    /// 序列号计数器
    seq_counter: AtomicU32,
    /// TDMA 中断到达标志
    irq_pending: AtomicBool,
    /// 外部 IRQ handler 命中次数
    irq_handler_hits: AtomicU64,
    /// MMIO 轮询兜底命中次数
    poll_fallback_hits: AtomicU64,
    /// 是否已经提示过兜底路径
    fallback_warned: AtomicBool,
    /// 轮询等待时的延时函数指针（0 表示未注入，退化为自旋）。
    delay_fn: AtomicUsize,
}

/// 等待 TDMA 完成时每轮轮询之间的延时（微秒）。
///
/// 注入延时函数后，`wait_irq` 每隔该间隔检查一次中断标志/硬件状态，
/// 而非空转自旋，从而既能精确计时又能给出有界的超时。
const WAIT_POLL_INTERVAL_US: u64 = 100;

impl Sg2002Tpu {
    /// 创建未初始化的 TPU 设备
    ///
    /// 使用默认物理地址，需要提供虚拟地址偏移
    ///
    /// # Safety
    /// 调用者必须确保偏移计算后的虚拟地址有效
    pub unsafe fn new() -> Self {
        let virt_offset = 0xffff_ffc0_0000_0000u64 as isize;
        let tdma_vaddr = (TDMA_PHYS_BASE as isize + virt_offset) as *mut u8;
        let tiu_vaddr = (TIU_PHYS_BASE as isize + virt_offset) as *mut u8;

        unsafe { Self::from_vaddr(tdma_vaddr, tiu_vaddr) }
    }

    /// 使用指定的虚拟地址创建 TPU 设备
    ///
    /// # Safety
    /// 调用者必须确保虚拟地址有效
    pub unsafe fn from_vaddr(tdma_vaddr: *mut u8, tiu_vaddr: *mut u8) -> Self {
        Self {
            tdma_vaddr,
            tiu_vaddr,
            inner: Mutex::new(TpuDeviceInner {
                tdma: unsafe { TdmaRegs::new(tdma_vaddr) },
                tiu: unsafe { TiuRegs::new(tiu_vaddr) },
                state: TpuState::Uninitialized,
                runtime: TpuRuntimeState::default(),
                kernel_work: TpuKernelWork::default(),
                tiu_irq_callback: None,
            }),
            seq_counter: AtomicU32::new(0),
            irq_pending: AtomicBool::new(false),
            irq_handler_hits: AtomicU64::new(0),
            poll_fallback_hits: AtomicU64::new(0),
            fallback_warned: AtomicBool::new(false),
            delay_fn: AtomicUsize::new(0),
        }
    }

    /// 注册 TIU 中断回调。
    ///
    /// 回调将在检测到 TIU BD 中断标志时被调用。
    pub fn register_tiu_irq_callback(&self, callback: TiuIrqCallback) {
        let mut inner = self.inner.lock();
        inner.tiu_irq_callback = Some(callback);
    }

    /// 清除 TIU 中断回调。
    pub fn clear_tiu_irq_callback(&self) {
        let mut inner = self.inner.lock();
        inner.tiu_irq_callback = None;
    }

    /// 注册轮询等待时的延时函数（由 OS glue 注入，见 [`DelayFn`]）。
    ///
    /// 注入后 `wait_irq` 会以 [`WAIT_POLL_INTERVAL_US`] 为间隔定时轮询，
    /// 而非空转自旋；未注入时退化为 `spin_loop`。
    pub fn set_delay_fn(&self, delay_fn: DelayFn) {
        self.delay_fn.store(delay_fn as usize, Ordering::Release);
    }

    /// 等待一轮轮询间隔：注入了延时函数则按 `usecs` 精确延时，否则自旋。
    fn wait_poll_interval(&self, usecs: u64) {
        let raw = self.delay_fn.load(Ordering::Acquire);
        if raw != 0 {
            // SAFETY: `delay_fn` 仅由 `set_delay_fn` 写入一个合法的 `DelayFn`
            // 函数指针，非零即有效。
            let delay: DelayFn = unsafe { core::mem::transmute::<usize, DelayFn>(raw) };
            delay(usecs);
        } else {
            core::hint::spin_loop();
        }
    }

    /// 初始化 TPU 设备 (probe)
    pub fn init(&self) -> Result<(), TpuError> {
        let mut inner = self.inner.lock();

        // 重置命令 ID
        super::platform::resync_cmd_id(&inner.tdma, &inner.tiu);

        inner.state = TpuState::Idle;
        inner.runtime = TpuRuntimeState::default();

        info!("TPU device initialized");
        Ok(())
    }

    /// 获取设备状态
    pub fn state(&self) -> TpuState {
        self.inner.lock().state
    }

    /// 检查设备是否就绪
    pub fn is_ready(&self) -> bool {
        self.inner.lock().state == TpuState::Idle
    }

    /// 处理 TDMA 中断
    ///
    /// 应该在你的 OS 中断处理程序中调用此函数
    ///
    /// 返回是否有错误发生
    pub fn handle_irq(&self) -> bool {
        let tdma = unsafe { TdmaRegs::new(self.tdma_vaddr) };
        let reg_value = tdma.read(super::tdma::TDMA_INT_MASK);
        let int_status = (reg_value >> 16) & !super::tdma::TDMA_MASK_INIT;
        if int_status == 0 {
            return false;
        }
        let has_error =
            int_status != super::tdma::TDMA_INT_EOD && int_status != super::tdma::TDMA_INT_EOPMU;
        tdma.clear_interrupt();
        self.irq_handler_hits.fetch_add(1, Ordering::AcqRel);
        self.irq_pending.store(true, Ordering::Release);
        has_error
    }

    /// 返回中断统计：(外部 IRQ 命中次数, MMIO 轮询兜底次数)。
    pub fn irq_stats(&self) -> (u64, u64) {
        (
            self.irq_handler_hits.load(Ordering::Acquire),
            self.poll_fallback_hits.load(Ordering::Acquire),
        )
    }

    /// 获取下一个序列号
    pub fn next_seq_no(&self) -> u32 {
        self.seq_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// 提交 DMA buffer 任务
    ///
    /// 调用方需先将 ioctl 中的 fd 解析为 `(vaddr, paddr)`。
    pub fn submit_dmabuf(
        &self,
        fd: i32,
        seq_no: u32,
        dmabuf_vaddr: usize,
        dmabuf_paddr: u64,
    ) -> Result<(), TpuError> {
        debug!("[TPU] submit dmabuf: fd={}, seq_no={}", fd, seq_no);
        debug!(
            "[TPU] Buffer: vaddr=0x{:x}, paddr=0x{:x}",
            dmabuf_vaddr, dmabuf_paddr
        );

        // 创建任务节点
        let task = TpuTaskNode {
            pid: 0, // 当前没有进程 ID 概念，可以后续扩展
            seq_no,
            dmabuf_fd: fd,
            dmabuf_vaddr,
            dmabuf_paddr,
            tpu_path: TpuSubmitPath::DesNormal,
            ret: 0,
        };

        // 添加到任务队列
        let mut inner = self.inner.lock();
        inner.kernel_work.task_list.push_back(task);

        // 直接执行任务 (简化版本，不使用工作线程)
        self.process_task_locked(&mut inner)?;

        Ok(())
    }

    /// 处理任务 (内部函数，需要持有锁)
    fn process_task_locked(&self, inner: &mut TpuDeviceInner) -> Result<(), TpuError> {
        while let Some(mut task) = inner.kernel_work.task_list.pop_front() {
            // 初始化 TPU
            super::platform::resync_cmd_id(&inner.tdma, &inner.tiu);
            inner.runtime.irq_received = false;

            // 执行 DMA buffer
            let result = self.run_dmabuf_internal(
                inner,
                task.seq_no,
                task.dmabuf_vaddr as *const u8,
                task.dmabuf_paddr,
            );

            task.ret = match result {
                Ok(_) => 0,
                Err(e) => {
                    error!("TPU run dmabuf failed: {:?}", e);
                    -1
                }
            };

            // 移动到完成队列
            inner.kernel_work.done_list.push_back(task);
        }

        Ok(())
    }

    /// 内部执行 DMA buffer
    fn run_dmabuf_internal(
        &self,
        inner: &mut TpuDeviceInner,
        seq_no: u32,
        dmabuf_vaddr: *const u8,
        dmabuf_paddr: u64,
    ) -> Result<(), TpuError> {
        if inner.state != TpuState::Idle && inner.state != TpuState::Uninitialized {
            return Err(TpuError::NotInitialized);
        }

        inner.state = TpuState::Running;

        // 简化版超时检查 (使用 Cell 实现内部可变性)
        let timeout_counter = Cell::new(0u64);
        let timeout_limit = 10_000_000_000u64; // 大约 10 秒
        // 等待 TDMA 完成的总超时（约 10 秒），以轮询间隔为步长。
        const WAIT_TIMEOUT_US: u64 = 10_000_000;
        let wait_poll_steps = WAIT_TIMEOUT_US / WAIT_POLL_INTERVAL_US;
        self.irq_pending.store(false, Ordering::Release);
        let tdma_irq_poll = unsafe { TdmaRegs::new(self.tdma_vaddr) };

        let wait_irq = || -> Result<(), TpuError> {
            // 优先等待外部 IRQ；每隔 `WAIT_POLL_INTERVAL_US` 检查一次，期间
            // 通过注入的延时函数精确计时，而非空转自旋。
            let mut steps = 0u64;
            while steps < wait_poll_steps {
                if self.irq_pending.swap(false, Ordering::AcqRel) {
                    return Ok(());
                }

                // 兜底：若外部 IRQ 未投递到内核，直接读取 TDMA 中断状态寄存器。
                let int_status = tdma_irq_poll.get_int_status();
                if int_status == super::tdma::TDMA_INT_EOD
                    || int_status == super::tdma::TDMA_INT_EOPMU
                {
                    tdma_irq_poll.clear_interrupt();
                    self.poll_fallback_hits.fetch_add(1, Ordering::AcqRel);
                    if self
                        .fallback_warned
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        warn!("[TPU] external IRQ path not observed yet, using MMIO poll fallback");
                    }
                    return Ok(());
                }

                self.wait_poll_interval(WAIT_POLL_INTERVAL_US);
                steps += 1;
            }
            Err(TpuError::Timeout)
        };

        let timeout_checker = || -> bool {
            let next = timeout_counter.get().saturating_add(1);
            timeout_counter.set(next);
            next > timeout_limit
        };

        // 使用指针避免同时借用
        let tdma = &inner.tdma as *const TdmaRegs;
        let tiu = &inner.tiu as *const TiuRegs;
        let tiu_irq_callback = inner.tiu_irq_callback;
        let runtime = &mut inner.runtime;
        runtime.current_seq_no = seq_no;
        runtime.tiu_irq_callback = tiu_irq_callback;

        let result = unsafe {
            super::platform::run_dmabuf(
                &*tdma,
                &*tiu,
                dmabuf_vaddr,
                dmabuf_paddr,
                runtime,
                wait_irq,
                timeout_checker,
            )
        };

        inner.state = TpuState::Idle;
        result
    }

    /// 等待 DMA buffer 完成。返回任务的 `ret` 值。
    ///
    /// 若找不到对应 `seq_no`，返回 `Err(TpuError::NotInitialized)`。
    pub fn wait_dmabuf(&self, seq_no: u32) -> Result<i32, TpuError> {
        debug!("TPU wait dmabuf: seq_no={}", seq_no);

        let mut inner = self.inner.lock();

        // 在完成队列中查找
        let mut found_idx = None;
        for (idx, task) in inner.kernel_work.done_list.iter().enumerate() {
            if task.seq_no == seq_no {
                found_idx = Some(idx);
                break;
            }
        }

        if let Some(idx) = found_idx {
            let task = inner.kernel_work.done_list.remove(idx).unwrap();
            debug!(
                "TPU wait dmabuf completed: seq_no={}, ret={}",
                seq_no, task.ret
            );
            Ok(task.ret)
        } else {
            warn!("TPU wait dmabuf: seq_no {} not found", seq_no);
            Err(TpuError::NotInitialized)
        }
    }

    /// 刷新 DMA buffer 缓存 (通过物理地址)
    pub fn cache_flush_paddr(&self, paddr: u64, size: u64) -> Result<(), TpuError> {
        debug!("TPU cache flush: paddr=0x{:x}, size={}", paddr, size);

        // 在 RISC-V 上执行 cache flush
        #[cfg(target_arch = "riscv64")]
        {
            // 使用 fence 指令确保内存一致性
            unsafe {
                core::arch::asm!("fence iorw, iorw");
            }
        }
        let _ = (paddr, size);

        Ok(())
    }

    /// 无效化 DMA buffer 缓存 (通过物理地址)
    pub fn cache_invalidate_paddr(&self, paddr: u64, size: u64) -> Result<(), TpuError> {
        debug!("TPU cache invalidate: paddr=0x{:x}, size={}", paddr, size);

        // 在 RISC-V 上执行 cache invalidate
        #[cfg(target_arch = "riscv64")]
        {
            unsafe {
                core::arch::asm!("fence iorw, iorw");
            }
        }
        let _ = (paddr, size);

        Ok(())
    }

    /// 挂起 TPU
    pub fn suspend(&self) -> Result<(), TpuError> {
        let mut inner = self.inner.lock();

        if inner.state == TpuState::Suspended {
            return Ok(());
        }

        // 使用指针避免同时借用
        let tdma = &inner.tdma as *const TdmaRegs;
        let tiu = &inner.tiu as *const TiuRegs;
        let reg_backup = &mut inner.runtime.reg_backup;
        unsafe {
            super::platform::backup_registers(&*tdma, &*tiu, reg_backup);
        }
        inner.state = TpuState::Suspended;

        info!("TPU suspended");
        Ok(())
    }

    /// 恢复 TPU
    pub fn resume(&self) -> Result<(), TpuError> {
        let mut inner = self.inner.lock();

        if inner.state != TpuState::Suspended {
            return Err(TpuError::NotInitialized);
        }

        // 使用指针避免同时借用
        let tdma = &inner.tdma as *const TdmaRegs;
        let tiu = &inner.tiu as *const TiuRegs;
        let reg_backup = &inner.runtime.reg_backup;
        unsafe {
            super::platform::restore_registers(&*tdma, &*tiu, reg_backup);
        }
        inner.state = TpuState::Idle;

        info!("TPU resumed");
        Ok(())
    }

    /// 重置 TPU
    pub fn reset(&self) {
        let mut inner = self.inner.lock();
        super::platform::resync_cmd_id(&inner.tdma, &inner.tiu);
        inner.runtime = TpuRuntimeState::default();
        inner.state = TpuState::Idle;

        info!("TPU reset");
    }
}

// 实现 Send 和 Sync
unsafe impl Send for Sg2002Tpu {}
unsafe impl Sync for Sg2002Tpu {}
