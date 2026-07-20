//! TPU 设备抽象（硬件层）
//!
//! 提供与 OS 解耦的高层 API，调用方负责将 fd / ioctl 解析为
//! `(seq_no, vaddr, paddr)` 后再调用本模块。

use core::{
    cell::Cell,
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use ax_kspin::SpinNoPreempt as Mutex;

use super::{
    TDMA_PHYS_BASE, TIU_PHYS_BASE,
    error::TpuError,
    platform::{TiuIrqCallback, TpuRuntimeState, WaitIrqFn},
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

/// Stable TDMA interrupt evidence captured and acknowledged by the IRQ endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TdmaIrqEvent {
    /// The submitted operation reached a supported terminal interrupt.
    Completed { status: u32 },
    /// TDMA reported a terminal interrupt other than a supported completion.
    Fault { status: u32 },
}

impl TdmaIrqEvent {
    /// Returns the raw status captured before acknowledgement.
    pub const fn status(self) -> u32 {
        match self {
            Self::Completed { status } | Self::Fault { status } => status,
        }
    }

    /// Returns whether this event represents a hardware failure.
    pub const fn is_fault(self) -> bool {
        matches!(self, Self::Fault { .. })
    }
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
    /// Monotonic publication generation of captured TDMA IRQ evidence.
    irq_generation: AtomicU64,
    /// Raw status associated with `irq_generation`.
    irq_status: AtomicU32,
    /// 外部 IRQ handler 命中次数
    irq_handler_hits: AtomicU64,
    /// Injected blocking wait capability; zero means activation is incomplete.
    wait_fn: AtomicUsize,
}

/// Absolute watchdog budget for one expected TDMA interrupt.
const IRQ_WATCHDOG_US: u64 = 10_000_000;

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
                tiu_irq_callback: None,
            }),
            seq_counter: AtomicU32::new(0),
            irq_generation: AtomicU64::new(0),
            irq_status: AtomicU32::new(0),
            irq_handler_hits: AtomicU64::new(0),
            wait_fn: AtomicUsize::new(0),
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

    /// 注册阻塞等待函数（由 OS glue 注入，见 [`WaitIrqFn`]）。
    ///
    /// `run_one` refuses to start until OS glue installs this capability. The
    /// waiter must keep one absolute deadline while ignoring unrelated wakes.
    pub fn set_wait_irq_fn(&self, wait_fn: WaitIrqFn) {
        self.wait_fn.store(wait_fn as usize, Ordering::Release);
    }

    fn require_irq_waiter(&self) -> Result<WaitIrqFn, TpuError> {
        let raw = self.wait_fn.load(Ordering::Acquire);
        if raw == 0 {
            return Err(TpuError::NotInitialized);
        }
        // SAFETY: `set_wait_irq_fn` is the only writer and stores a valid
        // `WaitIrqFn`. A non-zero value therefore reconstructs that function.
        Ok(unsafe { core::mem::transmute::<usize, WaitIrqFn>(raw) })
    }

    /// Returns the generation of the latest captured and acknowledged IRQ.
    pub fn irq_generation(&self) -> u64 {
        self.irq_generation.load(Ordering::Acquire)
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

    /// Captures and acknowledges one TDMA interrupt in hard-IRQ context.
    ///
    /// This is the only normal-I/O path that destructively reads the TDMA
    /// interrupt status. The returned value is stable ordinary-memory evidence
    /// suitable for publication by OS glue.
    pub fn capture_irq(&self) -> Option<TdmaIrqEvent> {
        let tdma = unsafe { TdmaRegs::new(self.tdma_vaddr) };
        let reg_value = tdma.read(super::tdma::TDMA_INT_MASK);
        let int_status = (reg_value >> 16) & !super::tdma::TDMA_MASK_INIT;
        if int_status == 0 {
            return None;
        }
        tdma.clear_interrupt();
        let event = if int_status == super::tdma::TDMA_INT_EOD
            || int_status == super::tdma::TDMA_INT_EOPMU
        {
            TdmaIrqEvent::Completed { status: int_status }
        } else {
            TdmaIrqEvent::Fault { status: int_status }
        };

        // The endpoint is single-owner, so status is published before the
        // monotonically increasing generation. Acquire readers that observe
        // the generation also observe its matching status.
        self.irq_status.store(int_status, Ordering::Relaxed);
        let next_generation = self.irq_generation.load(Ordering::Relaxed).wrapping_add(1);
        self.irq_generation
            .store(next_generation, Ordering::Release);
        self.irq_handler_hits.fetch_add(1, Ordering::AcqRel);
        Some(event)
    }

    /// Returns the number of interrupts captured by the external IRQ endpoint.
    pub fn irq_handler_hits(&self) -> u64 {
        self.irq_handler_hits.load(Ordering::Acquire)
    }

    /// 获取下一个序列号
    pub fn next_seq_no(&self) -> u32 {
        self.seq_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// 阻塞执行一次推理。**由 OS glue 的 worker 线程调用**。
    ///
    /// 调用方需先将 ioctl 中的 fd 解析为 `(vaddr, paddr)`。本函数会一直阻塞
    /// 到该 dmabuf 推理完成（内部可能多段 fire→等中断→检查），其间等待硬件
    /// 时通过注入的 [`WaitIrqFn`] 睡眠让出 CPU。
    ///
    /// 不在等待硬件期间持有 `inner` 自旋锁：依赖单 worker 串行访问硬件这一
    /// 前提，寄存器从 `tdma_vaddr`/`tiu_vaddr` 局部重建（同 `capture_irq`），
    /// 运行时状态放栈上，仅在状态翻转/读回调时短暂持锁。
    pub fn run_one(
        &self,
        seq_no: u32,
        dmabuf_vaddr: usize,
        dmabuf_paddr: u64,
    ) -> Result<(), TpuError> {
        debug!(
            "[TPU] run_one: seq_no={}, vaddr=0x{:x}, paddr=0x{:x}",
            seq_no, dmabuf_vaddr, dmabuf_paddr
        );
        let wait_irq_capability = self.require_irq_waiter()?;

        // 仅短暂持锁：校验/翻转状态并取出 TIU 回调，随后立即释放，
        // 不在等待硬件期间持锁（否则 worker 无法睡眠让出 CPU）。
        let tiu_irq_callback = {
            let mut inner = self.inner.lock();
            if inner.state != TpuState::Idle && inner.state != TpuState::Uninitialized {
                return Err(TpuError::NotInitialized);
            }
            inner.state = TpuState::Running;
            inner.tiu_irq_callback
        };

        // 寄存器为纯 MMIO vaddr 包装，单 worker 串行访问，无需持锁重建。
        let tdma = unsafe { TdmaRegs::new(self.tdma_vaddr) };
        let tiu = unsafe { TiuRegs::new(self.tiu_vaddr) };

        // 运行时状态放栈上：worker 是唯一访问者，避免借用 vs 锁的张力。
        let mut runtime = TpuRuntimeState {
            current_seq_no: seq_no,
            tiu_irq_callback,
            ..TpuRuntimeState::default()
        };

        // 简化版超时检查 (使用 Cell 实现内部可变性)
        let timeout_counter = Cell::new(0u64);
        let timeout_limit = 10_000_000_000u64; // 大约 10 秒
        let observed_irq_generation = Cell::new(self.irq_generation());

        let wait_irq = || -> Result<(), TpuError> {
            let observed = observed_irq_generation.get();
            let mut captured = self.irq_generation();
            if captured == observed {
                if !wait_irq_capability(observed, IRQ_WATCHDOG_US) {
                    return Err(TpuError::Timeout);
                }
                captured = self.irq_generation();
            }
            if captured == observed {
                return Err(TpuError::Timeout);
            }

            let status = self.irq_status.load(Ordering::Acquire);
            observed_irq_generation.set(captured);
            if status == super::tdma::TDMA_INT_EOD || status == super::tdma::TDMA_INT_EOPMU {
                Ok(())
            } else {
                Err(TpuError::TdmaError(status))
            }
        };

        let timeout_checker = || -> bool {
            let next = timeout_counter.get().saturating_add(1);
            timeout_counter.set(next);
            next > timeout_limit
        };

        let result = unsafe {
            super::platform::run_dmabuf(
                &tdma,
                &tiu,
                dmabuf_vaddr as *const u8,
                dmabuf_paddr,
                &mut runtime,
                wait_irq,
                timeout_checker,
            )
        };

        {
            let mut inner = self.inner.lock();
            inner.runtime = runtime;
            inner.state = TpuState::Idle;
        }

        if let Err(e) = &result {
            error!("TPU run_one failed: seq_no={}, err={:?}", seq_no, e);
        }
        result
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
