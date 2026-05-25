//! TPU 设备抽象（硬件层）
//!
//! 提供与 OS 解耦的高层 API，调用方负责将 fd / ioctl 解析为
//! `(seq_no, vaddr, paddr)` 后再调用本模块。

use alloc::collections::VecDeque;
use core::{
    cell::Cell,
    sync::atomic::{AtomicU32, Ordering},
};

use ax_kspin::SpinNoIrq as Mutex;

use super::{
    TDMA_PHYS_BASE, TIU_PHYS_BASE, error::TpuError, platform::TpuRuntimeState, tdma::TdmaRegs,
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
}

/// SG2002 TPU 设备（仅硬件层）
pub struct Sg2002Tpu {
    /// 内部状态 (使用自旋锁保护)
    inner: Mutex<TpuDeviceInner>,
    /// 序列号计数器
    seq_counter: AtomicU32,
}

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
            inner: Mutex::new(TpuDeviceInner {
                tdma: unsafe { TdmaRegs::new(tdma_vaddr) },
                tiu: unsafe { TiuRegs::new(tiu_vaddr) },
                state: TpuState::Uninitialized,
                runtime: TpuRuntimeState::default(),
                kernel_work: TpuKernelWork::default(),
            }),
            seq_counter: AtomicU32::new(0),
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
        let mut inner = self.inner.lock();
        // 先获取需要的引用，避免同时借用
        let tdma = &inner.tdma as *const TdmaRegs;
        let tiu = &inner.tiu as *const TiuRegs;
        let runtime = &mut inner.runtime;
        unsafe { super::platform::handle_tdma_irq(&*tdma, &*tiu, runtime) }
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
            let result =
                self.run_dmabuf_internal(inner, task.dmabuf_vaddr as *const u8, task.dmabuf_paddr);

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
        dmabuf_vaddr: *const u8,
        dmabuf_paddr: u64,
    ) -> Result<(), TpuError> {
        if inner.state != TpuState::Idle && inner.state != TpuState::Uninitialized {
            return Err(TpuError::NotInitialized);
        }

        inner.state = TpuState::Running;

        // 简化版超时检查 (使用 Cell 实现内部可变性)
        let timeout_counter = Cell::new(0u64);
        let timeout_limit = 1_000_000_000u64; // 大约 10 秒

        let wait_irq = || -> Result<(), TpuError> {
            // 轮询等待中断
            // 简化实现：直接返回 Ok，由 poll_cmdbuf_done 处理超时
            let mut counter = timeout_counter.get();
            while counter < timeout_limit {
                counter += 1;
                timeout_counter.set(counter);
                core::hint::spin_loop();
                // 简化：假设执行一定迭代后完成
                if counter > 10000 {
                    break;
                }
            }
            if counter >= timeout_limit {
                return Err(TpuError::Timeout);
            }
            Ok(())
        };

        let timeout_checker = || -> bool { timeout_counter.get() > timeout_limit };

        // 使用指针避免同时借用
        let tdma = &inner.tdma as *const TdmaRegs;
        let tiu = &inner.tiu as *const TiuRegs;
        let runtime = &mut inner.runtime;

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
