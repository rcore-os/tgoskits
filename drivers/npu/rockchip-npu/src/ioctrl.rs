use alloc::vec::Vec;
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use tock_registers::interfaces::Readable;

use crate::{
    JobMode, RKNPU_CORE_AUTO_MASK, RKNPU_CORE0_MASK, RKNPU_CORE1_MASK, RKNPU_CORE2_MASK, Rknpu,
    RknpuError, RknpuTask, SubmitBase, SubmitRef, registers::rknpu_fuzz_status,
};

const RKNN_NPU_CORE_ALL: u32 = 0xffff;
const RKNPU_SYNC_POLL_LOG_INTERVAL: u64 = 1_000_000;
const NANOS_PER_MILLISECOND: u64 = 1_000_000;
static LOGGED_SUBMIT_CORE_LAYOUT: AtomicBool = AtomicBool::new(false);

/// 子核心任务索引结构体
///
/// 对应 C 结构体 `rknpu_subcore_task`
/// 用于表示子核心任务的起始索引和任务数量
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RknpuSubcoreTask {
    /// 任务起始索引
    pub task_start: u32,
    /// 任务数量
    pub task_number: u32,
}

/// A structure for getting a fake-offset that can be used with mmap.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
pub struct RknpuMemMap {
    /// handle of gem object.
    pub handle: u32,
    /// just padding to be 64-bit aligned.
    pub reserved: u32,
    /// a fake-offset of gem object.
    pub offset: u64,
}

/// Arguments for destroying a GEM object (releasing its handle).
///
/// Corresponds to C `struct rknpu_mem_destroy`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
pub struct RknpuMemDestroy {
    /// handle of the gem object to destroy.
    pub handle: u32,
    /// just padding to be 64-bit aligned.
    pub reserved: u32,
    /// address of the RKNPU memory object (informational; the handle identifies it).
    pub obj_addr: u64,
}

/// 任务提交结构体
///
/// 对应 C 结构体 `rknpu_submit`
/// 用于向 RKNPU 提交作业任务
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
pub struct RknpuSubmit {
    /// 作业提交标志
    pub flags: u32,
    /// Submission timeout in milliseconds, matching Linux rknpu_job_wait.
    pub timeout: u32,
    /// 任务起始索引
    pub task_start: u32,
    /// 任务数量
    pub task_number: u32,
    /// 任务计数器
    pub task_counter: u32,
    /// 提交优先级
    pub priority: i32,
    /// 任务对象地址
    pub task_obj_addr: u64,
    /// IOMMU 域 ID
    pub iommu_domain_id: u32,
    /// 保留字段（64位对齐）
    pub reserved: u32,
    /// 任务基地址
    pub task_base_addr: u64,
    /// 硬件运行时间
    pub hw_elapse_time: i64,
    /// RKNPU 核心掩码
    pub core_mask: u32,
    /// DMA 信号量文件描述符
    pub fence_fd: i32,
    /// 子核心任务数组（固定大小为5）
    pub subcore_task: [RknpuSubcoreTask; 5],
}

/// User-desired buffer creation information structure.
///
/// Fields correspond to the original C layout. Use `#[repr(C)]` so this type
/// can be used across the FFI boundary or when mirroring kernel structs.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
pub struct RknpuMemCreate {
    /// The handle of the created GEM object.
    pub handle: u32,
    /// User request for setting memory type or cache attributes.
    pub flags: u32,
    /// User-desired memory allocation size (page-aligned by caller).
    pub size: u64,
    /// Address of RKNPU memory object.
    pub obj_addr: u64,
    /// DMA address that is accessible by the RKNPU.
    pub dma_addr: u64,
    /// User-desired SRAM memory allocation size (page-aligned by caller).
    pub sram_size: u64,
    /// IOMMU domain id.
    pub iommu_domain_id: i32,
    /// Core mask (reserved/padding in original structure).
    pub core_mask: u32,
}

/// For synchronizing DMA buffer
///
/// Fields correspond to the original C layout. Use `#[repr(C)]` so this type
/// can be used across FFI boundaries if needed.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
pub struct RknpuMemSync {
    /// User request for setting memory type or cache attributes.
    pub flags: u32,
    /// Reserved for padding.
    pub reserved: u32,
    /// Address of RKNPU memory object.
    pub obj_addr: u64,
    /// Offset in bytes from start address of buffer.
    pub offset: u64,
    /// Size of memory region.
    pub size: u64,
}

#[derive(Debug)]
struct CoreSubmitState {
    core_idx: usize,
    task_iter: usize,
    task_end: usize,
    current_start: usize,
    current_number: usize,
    current_int_mask: u32,
    completed: usize,
    inflight: bool,
}

/// A monotonic deadline shared by all polling phases of one submission.
#[derive(Debug, Clone, Copy)]
struct SubmitDeadline {
    expires_at_ns: u64,
}

impl SubmitDeadline {
    fn from_timeout(start_ns: u64, timeout_ms: u32) -> Self {
        Self {
            expires_at_ns: start_ns.saturating_add(u64::from(timeout_ms) * NANOS_PER_MILLISECOND),
        }
    }

    fn expired(self, now_ns: u64) -> bool {
        now_ns >= self.expires_at_ns
    }
}

/// Polls an MMIO condition until it becomes ready or the submission expires.
///
/// The clock is supplied by OS glue so this portable driver core does not depend
/// on a particular runtime timer implementation.
fn poll_until_ready<T>(
    deadline: SubmitDeadline,
    clock: &mut impl FnMut() -> u64,
    mut poll: impl FnMut() -> Result<Option<T>, RknpuError>,
) -> Result<T, RknpuError> {
    loop {
        if let Some(value) = poll()? {
            return Ok(value);
        }
        if deadline.expired(clock()) {
            return Err(RknpuError::Timeout);
        }
        spin_loop();
    }
}

fn core_mask_for_index(core_idx: usize) -> u32 {
    match core_idx {
        0 => RKNPU_CORE0_MASK,
        1 => RKNPU_CORE1_MASK,
        2 => RKNPU_CORE2_MASK,
        _ => 0,
    }
}

fn active_core_count(core_mask: u32) -> usize {
    ((core_mask & RKNPU_CORE0_MASK != 0) as usize)
        + ((core_mask & RKNPU_CORE1_MASK != 0) as usize)
        + ((core_mask & RKNPU_CORE2_MASK != 0) as usize)
}

fn is_supported_core_mask(core_mask: u32) -> bool {
    match core_mask {
        RKNPU_CORE0_MASK | RKNPU_CORE1_MASK | RKNPU_CORE2_MASK => true,
        mask if mask == RKNPU_CORE0_MASK | RKNPU_CORE1_MASK => true,
        mask if mask == RKNPU_CORE0_MASK | RKNPU_CORE1_MASK | RKNPU_CORE2_MASK => true,
        _ => false,
    }
}

fn subcore_task_index(use_core_num: usize, core_idx: usize) -> usize {
    if use_core_num == 3 {
        core_idx + 2
    } else {
        core_idx
    }
}

impl Rknpu {
    /// Submits an RKNPU job and bounds all synchronous polling by `args.timeout`.
    ///
    /// `clock` must return monotonically nondecreasing nanoseconds. The timeout
    /// encoded in [`RknpuSubmit`] is measured in milliseconds by the RKNPU ABI.
    pub fn submit_ioctrl(
        &mut self,
        args: &mut RknpuSubmit,
        clock: &mut impl FnMut() -> u64,
    ) -> Result<(), RknpuError> {
        if args.flags & 1 << 1 > 0 {
            debug!("Nonblock task");
        }

        let core_mask = self.normalize_core_mask(args)?;
        if core_mask > self.data.core_mask || !is_supported_core_mask(core_mask) {
            warn!(
                "rknpu submit: invalid core_mask={:#x}, supported_mask={:#x}",
                args.core_mask, self.data.core_mask
            );
            return Err(RknpuError::InvalidParameter);
        }

        let use_core_num = active_core_count(core_mask);
        let mut states = Vec::new();
        for core_idx in 0..3 {
            if core_mask & core_mask_for_index(core_idx) == 0 {
                continue;
            }
            if self.base.get(core_idx).is_none() {
                warn!(
                    "rknpu submit: core {} requested by mask {:#x}, but only {} MMIO bases mapped",
                    core_idx,
                    core_mask,
                    self.base.len()
                );
                return Err(RknpuError::InvalidParameter);
            }
            let subcore_idx = subcore_task_index(use_core_num, core_idx);
            let subcore = args
                .subcore_task
                .get(subcore_idx)
                .ok_or(RknpuError::InvalidParameter)?;
            let (task_start, task_number) = if subcore.task_number != 0 {
                (subcore.task_start, subcore.task_number)
            } else if use_core_num == 1 && args.task_number != 0 {
                (args.task_start, args.task_number)
            } else {
                warn!(
                    "rknpu submit: core {} requested by mask {:#x}, but subcore_task[{}] is empty",
                    core_idx, core_mask, subcore_idx
                );
                return Err(RknpuError::InvalidParameter);
            };
            let task_start =
                usize::try_from(task_start).map_err(|_| RknpuError::InvalidParameter)?;
            let task_number =
                usize::try_from(task_number).map_err(|_| RknpuError::InvalidParameter)?;
            let task_end = task_start
                .checked_add(task_number)
                .ok_or(RknpuError::InvalidParameter)?;
            states.push(CoreSubmitState {
                core_idx,
                task_iter: task_start,
                task_end,
                current_start: 0,
                current_number: 0,
                current_int_mask: 0,
                completed: 0,
                inflight: false,
            });
        }

        if states.is_empty() {
            warn!(
                "rknpu submit: no active cores for core_mask={:#x}",
                args.core_mask
            );
            return Err(RknpuError::InvalidParameter);
        }

        if !LOGGED_SUBMIT_CORE_LAYOUT.swap(true, Ordering::Relaxed) {
            warn!(
                "rknpu submit: core_mask={:#x} active_cores={} subcore_layout={:?}",
                core_mask, use_core_num, states
            );
        }

        let deadline = SubmitDeadline::from_timeout(clock(), args.timeout);
        for state in states.iter_mut() {
            self.clear_pending_interrupts(state.core_idx, deadline, clock)?;
            self.submit_next_chunk(state, args)?;
        }

        let mut wait_count: u64 = 0;
        poll_until_ready(deadline, clock, || {
            let mut progressed = false;
            for state in states.iter_mut().filter(|state| state.inflight) {
                progressed |= self.poll_core_completion(state, args)?;
            }
            if progressed {
                wait_count = 0;
            } else {
                wait_count += 1;
                if wait_count.is_multiple_of(RKNPU_SYNC_POLL_LOG_INTERVAL) {
                    warn!(
                        "rknpu submit: still waiting for core_mask={:#x}, polled {} times",
                        core_mask, wait_count
                    );
                }
            }
            Ok((!states.iter().any(|state| state.inflight)).then_some(()))
        })?;

        args.task_counter = args.task_number;
        args.hw_elapse_time = (args.timeout / 2) as _;

        Ok(())
    }

    fn normalize_core_mask(&mut self, args: &RknpuSubmit) -> Result<u32, RknpuError> {
        match args.core_mask {
            RKNPU_CORE_AUTO_MASK => self.select_auto_core_mask(),
            RKNN_NPU_CORE_ALL => Ok(self.data.core_mask),
            mask if mask > self.data.core_mask => Err(RknpuError::InvalidParameter),
            mask => Ok(mask),
        }
    }

    fn select_auto_core_mask(&mut self) -> Result<u32, RknpuError> {
        if self.data.core_mask == 0 {
            return Err(RknpuError::InvalidParameter);
        }

        let base_index = self.auto_core_cursor % 3;
        self.auto_core_cursor = self.auto_core_cursor.wrapping_add(1);
        for offset in 0..3 {
            let core_idx = (base_index + offset) % 3;
            let mask = core_mask_for_index(core_idx);
            if self.data.core_mask & mask != 0 {
                return Ok(mask);
            }
        }

        Err(RknpuError::InvalidParameter)
    }

    fn clear_pending_interrupts(
        &mut self,
        core_idx: usize,
        deadline: SubmitDeadline,
        clock: &mut impl FnMut() -> u64,
    ) -> Result<(), RknpuError> {
        let mut clear_count: u64 = 0;
        poll_until_ready(deadline, clock, || {
            if self.base[core_idx].handle_interrupt() == 0 {
                return Ok(Some(()));
            }
            clear_count += 1;
            if clear_count.is_multiple_of(RKNPU_SYNC_POLL_LOG_INTERVAL) {
                warn!(
                    "rknpu submit: stuck clearing core {} interrupts, cleared {} times",
                    core_idx, clear_count
                );
            }
            Ok(None)
        })
    }

    fn submit_next_chunk(
        &mut self,
        state: &mut CoreSubmitState,
        args: &mut RknpuSubmit,
    ) -> Result<(), RknpuError> {
        let task_ptr = args.task_obj_addr as *mut RknpuTask;
        if task_ptr.is_null() {
            return Err(RknpuError::InvalidParameter);
        }
        let max_submit_number = self.data.max_submit_number as usize;

        let task_number = (state.task_end - state.task_iter).min(max_submit_number);
        let submit_tasks =
            unsafe { core::slice::from_raw_parts_mut(task_ptr.add(state.task_iter), task_number) };

        let job = SubmitRef {
            base: SubmitBase {
                flags: JobMode::from_bits_retain(args.flags),
                task_base_addr: args.task_base_addr as _,
                core_idx: state.core_idx,
                int_mask: submit_tasks.last().unwrap().int_mask,
                int_clear: submit_tasks[0].int_mask,
                regcfg_amount: submit_tasks[0].regcfg_amount,
            },
            task_number,
            regcmd_base_addr: submit_tasks[0].regcmd_addr as _,
        };
        debug!(
            "Submit {task_number} jobs on core {}: {job:#x?}",
            state.core_idx
        );
        self.base[state.core_idx].submit_pc(&self.data, &job)?;

        state.current_start = state.task_iter;
        state.current_number = task_number;
        state.current_int_mask = job.base.int_mask;
        state.task_iter += task_number;
        state.inflight = true;

        Ok(())
    }

    fn poll_core_completion(
        &mut self,
        state: &mut CoreSubmitState,
        args: &mut RknpuSubmit,
    ) -> Result<bool, RknpuError> {
        let status = self.base[state.core_idx].pc().interrupt_status.get();
        let status = rknpu_fuzz_status(status);

        if status != state.current_int_mask {
            if status != 0 {
                warn!(
                    "rknpu submit: core {} unexpected interrupt status={:#x}, int_mask={:#x}",
                    state.core_idx, status, state.current_int_mask
                );
                return Err(RknpuError::TaskError);
            }
            return Ok(false);
        }

        let int_status = status;
        self.base[state.core_idx].pc().clean_interrupts();

        let task_ptr = args.task_obj_addr as *mut RknpuTask;
        if task_ptr.is_null() || state.current_number == 0 {
            return Err(RknpuError::InvalidParameter);
        }
        let last_task_index = state.current_start + state.current_number - 1;
        unsafe {
            (*task_ptr.add(last_task_index)).int_status = int_status;
        }
        state.completed = state.completed.saturating_add(state.current_number);

        if state.task_iter < state.task_end {
            self.submit_next_chunk(state, args)?;
        } else {
            state.inflight = false;
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_within_abi_millisecond_timeout_is_not_rejected() {
        // Linux rknpu_job_wait passes timeout directly to msecs_to_jiffies.
        // A 6000 ms request must still accept completion after seven ms.
        let deadline = SubmitDeadline::from_timeout(0, 6000);
        let mut polls = 0;
        let result = poll_until_ready(deadline, &mut || 7_000_000, || {
            polls += 1;
            Ok((polls == 2).then_some(()))
        });
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn polling_never_ready_returns_timeout_at_deadline() {
        let deadline = SubmitDeadline::from_timeout(0, 1);
        let mut now_ns = 0;
        let mut poll_count = 0;

        let result: Result<(), RknpuError> = poll_until_ready(
            deadline,
            &mut || {
                now_ns += NANOS_PER_MILLISECOND;
                now_ns
            },
            || {
                poll_count += 1;
                Ok(None)
            },
        );

        assert_eq!(result, Err(RknpuError::Timeout));
        assert_eq!(poll_count, 1);
    }

    #[test]
    fn polling_accepts_completion_observed_at_deadline() {
        let deadline = SubmitDeadline::from_timeout(0, 1);
        let now_ns = NANOS_PER_MILLISECOND;

        let result = poll_until_ready(deadline, &mut || now_ns, || Ok(Some(())));

        assert_eq!(result, Ok(()));
    }
}
