use core::hint::spin_loop;

use mbarrier::mb;
use tock_registers::interfaces::Readable;

use crate::{
    JobMode, Rknpu, RknpuError, RknpuTask, SubmitBase, SubmitRef, registers::rknpu_fuzz_status,
};

/// 子核心任务索引结构体
///
/// 对应 C 结构体 `rknpu_subcore_task`
/// 用于表示子核心任务的起始索引和任务数量
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct RknpuSubcoreTask {
    /// 任务起始索引
    pub task_start: u32,
    /// 任务数量
    pub task_number: u32,
}

/// A structure for getting a fake-offset that can be used with mmap.
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct RknpuMemMap {
    /// handle of gem object.
    pub handle: u32,
    /// just padding to be 64-bit aligned.
    pub reserved: u32,
    /// a fake-offset of gem object.
    pub offset: u64,
}

/// 任务提交结构体
///
/// 对应 C 结构体 `rknpu_submit`
/// 用于向 RKNPU 提交作业任务
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct RknpuSubmit {
    /// 作业提交标志
    pub flags: u32,
    /// 提交超时时间
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
#[derive(Debug, Clone, Default)]
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
#[derive(Debug, Clone, Copy, Default)]
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

impl Rknpu {
    // pub fn submit_ioctrl(&mut self, args: &mut RknpuSubmit) -> Result<(), RknpuError> {
    //     self.gem.comfirm_write_all()?;
    //     let mut int_status = 0;

    //     if args.flags & 1 << 1 > 0 {
    //         debug!("Nonblock task");
    //     }
    //     let task_ptr = args.task_obj_addr as *mut RknpuTask;
    //     let mut task_iter = args.task_start as usize;
    //     let task_iter_end = task_iter + args.task_number as usize;
    //     let max_submit_number = self.data.max_submit_number as usize;

    //     while task_iter < task_iter_end {
    //         let task_number = (task_iter_end - task_iter).min(max_submit_number);
    //         let submit_tasks =
    //             unsafe { core::slice::from_raw_parts_mut(task_ptr.add(task_iter), task_number) };

    //         let job = SubmitRef {
    //             base: SubmitBase {
    //                 flags: JobMode::from_bits_retain(args.flags),
    //                 task_base_addr: args.task_base_addr as _,
    //                 core_idx: args.core_mask.trailing_zeros() as usize,
    //                 // core_idx: 0x0,
    //                 int_mask: submit_tasks.last().unwrap().int_mask,
    //                 int_clear: submit_tasks[0].int_mask,
    //                 regcfg_amount: submit_tasks[0].regcfg_amount,
    //             },
    //             task_number,
    //             regcmd_base_addr: submit_tasks[0].regcmd_addr as _,
    //         };
    //         debug!("Submit {task_number} jobs: {job:#x?}");
    //         while self.base[0].handle_interrupt() != 0 {
    //             spin_loop();
    //         }
    //         debug!("Submitting PC job...");
    //         self.base[0].submit_pc(&self.data, &job).unwrap();

    //         // Wait for completion
    //         loop {
    //             let status = self.base[0].pc().interrupt_status.get();
    //             let status = rknpu_fuzz_status(status);

    //             if status & job.base.int_mask > 0 {
    //                 int_status = job.base.int_mask & status;
    //                 break;
    //             }
    //             if status != 0 {
    //                 debug!("Interrupt status changed: {:#x}", status);
    //                 return Err(RknpuError::TaskError);
    //             }
    //         }
    //         self.base[0].pc().clean_interrupts();
    //         debug!("Job completed");
    //         submit_tasks.last_mut().unwrap().int_status = int_status;
    //         task_iter += task_number;
    //     }
    //     self.gem.prepare_read_all()?;

    //     args.task_counter = args.task_number as _;
    //     args.hw_elapse_time = (args.timeout / 2) as _;

    //     Ok(())
    // }
    pub fn submit_ioctrl(&mut self, args: &mut RknpuSubmit) -> Result<(), RknpuError> {
        self.gem.comfirm_write_all()?;

        if args.flags & 1 << 1 > 0 {
            debug!("Nonblock task");
        }

        for idx in 0..5 {
            if args.subcore_task[idx].task_number == 0 {
                continue;
            }
            debug!("Submitting subcore task index: {}", idx);
            let submitted_tasks = self.submit_one(idx, args)?;
            debug!(
                "Submitted {} tasks for subcore index {}",
                submitted_tasks, idx
            );
        }

        self.gem.prepare_read_all()?;

        args.task_counter = args.task_number as _;
        args.hw_elapse_time = (args.timeout / 2) as _;

        Ok(())
    }
    fn submit_one(&mut self, idx: usize, args: &mut RknpuSubmit) -> Result<usize, RknpuError> {
        let task_ptr = args.task_obj_addr as *mut RknpuTask;
        let subcore = &args.subcore_task[idx];

        let mut task_iter = subcore.task_start as usize;
        let task_iter_end = task_iter + subcore.task_number as usize;
        let max_submit_number = self.data.max_submit_number as usize;

        while task_iter < task_iter_end {
            let task_number = (task_iter_end - task_iter).min(max_submit_number);
            let submit_tasks =
                unsafe { core::slice::from_raw_parts_mut(task_ptr.add(task_iter), task_number) };

            let job = SubmitRef {
                base: SubmitBase {
                    flags: JobMode::from_bits_retain(args.flags),
                    task_base_addr: args.task_base_addr as _,
                    core_idx: idx,
                    int_mask: submit_tasks.last().unwrap().int_mask,
                    int_clear: submit_tasks[0].int_mask,
                    regcfg_amount: submit_tasks[0].regcfg_amount,
                },
                task_number,
                regcmd_base_addr: submit_tasks[0].regcmd_addr as _,
            };
            debug!("Submit {task_number} jobs: {job:#x?}");
            while self.base[idx].handle_interrupt() != 0 {
                spin_loop();
            }
            debug!("Submitting PC job...");
            self.base[idx].submit_pc(&self.data, &job).unwrap();
            let int_status;
            // Wait for completion
            loop {
                let status = self.base[idx].pc().interrupt_status.get();
                let status = rknpu_fuzz_status(status);

                if status & job.base.int_mask > 0 {
                    int_status = job.base.int_mask & status;
                    break;
                }
                if status != 0 {
                    debug!("Interrupt status changed: {:#x}", status);
                    return Err(RknpuError::TaskError);
                }
            }
            self.base[idx].pc().clean_interrupts();
            debug!("Job completed");
            submit_tasks.last_mut().unwrap().int_status = int_status;
            task_iter += task_number;
        }

        Ok(subcore.task_number as usize)
    }
}
