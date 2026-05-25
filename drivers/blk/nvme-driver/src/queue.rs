use core::{
    hint::spin_loop,
    mem,
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

use dma_api::{CoherentArray, DeviceDma};
use log::debug;
use tock_registers::register_bitfields;

use crate::{
    command::{self, Feature},
    err::*,
    registers::NvmeReg,
};

static ID_FACTORY: AtomicU32 = AtomicU32::new(0);

fn next_id() -> u32 {
    if ID_FACTORY
        .compare_exchange(0xFFFF, 0, Ordering::Relaxed, Ordering::Acquire)
        .is_ok()
    {
        return 0;
    }

    ID_FACTORY.fetch_add(1, Ordering::Relaxed)
}

register_bitfields! [
    u32,
    pub CommandDword0 [
        Opcode OFFSET(0) NUMBITS(8) [],
        FusedOperation OFFSET(8) NUMBITS(2) [
            Normal = 0,
            FusedFirst = 0b1,
            FusedSecond = 0b10,
            Reserved = 0b11,
        ],
        PSDT OFFSET(14) NUMBITS(2) [
            PRP = 0,
            SGLSignal = 0b1,
            SGLExactly = 0b10,
            Reserved = 0b11,
        ],
        CommandId OFFSET(16) NUMBITS(16) []
    ],
];

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct NvmeSubmission([u8; 64]);

pub trait Submission {
    fn to_submission(self) -> NvmeSubmission;
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
// 64B
pub struct CommandSet {
    pub cdw0: u32,
    pub nsid: u32,
    pub cdw2: [u32; 2],
    pub metadata: u64,
    pub prp1: u64,
    pub prp2: u64,
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
    pub cdw13: u32,
    pub cdw14: u32,
    pub cdw15: u32,
}

impl CommandSet {
    pub fn cdw0_from_opcode(opcode: command::Opcode) -> u32 {
        (CommandDword0::Opcode.val(opcode.as_u32()) + CommandDword0::CommandId.val(next_id())).value
    }

    pub fn set_features(feature: Feature) -> Self {
        let cdw0 = Self::cdw0_from_opcode(command::Opcode::SET_FEATURES);

        let cdw10 = feature.to_cdw10();
        let mut cdw11 = 0;
        match feature {
            Feature::NumberOfQueues { nsq, ncq } => cdw11 = nsq | ncq << 16,
            Feature::InterruptVectorConfiguration {} => {}
        };

        Self {
            cdw0,
            cdw10,
            cdw11,
            ..Default::default()
        }
    }

    pub fn create_io_completion_queue(
        qid: u32,
        size: u32,
        paddr: u64,
        physically_contiguous: bool,
        interrupts_enabled: bool,
        interrupt_vector: u32,
    ) -> CommandSet {
        let cdw0 = Self::cdw0_from_opcode(command::Opcode::CREATE_IO_CQ);
        let prp1 = paddr;
        let cdw10 = (qid & 0xffff) | ((size - 1) & 0xffff) << 16;

        let cdw11 = if physically_contiguous { 1 } else { 0 }
            | if interrupts_enabled { 1 << 1 } else { 0 }
            | interrupt_vector << 16;

        CommandSet {
            cdw0,
            prp1,
            cdw10,
            cdw11,
            ..Default::default()
        }
    }

    pub fn create_io_submission_queue(
        qid: u32,
        size: u32,
        paddr: u64,
        physically_contiguous: bool,
        priority: u32,
        cqid: u32,
        nvm_set_id: u16,
    ) -> CommandSet {
        let cdw0 = Self::cdw0_from_opcode(command::Opcode::CREATE_IO_SQ);
        let prp1 = paddr;
        let cdw10 = (qid & 0xffff) | ((size - 1) & 0xffff) << 16;
        let cdw11 = if physically_contiguous { 1 } else { 0 } | priority << 1 | cqid << 16;

        CommandSet {
            cdw0,
            prp1,
            cdw10,
            cdw11,
            cdw12: nvm_set_id as _,
            ..Default::default()
        }
    }

    pub fn nvm_cmd_read(nsid: u32, paddr: u64, starting_lba: u64, blk_num: u16) -> Self {
        let cdw0 = Self::cdw0_from_opcode(command::Opcode::NVM_READ);
        let low = (starting_lba & 0xFFFFFFFF) as u32;
        let high = (starting_lba >> 32) as u32;
        let cdw12 = blk_num as u32;

        CommandSet {
            nsid,
            cdw0,
            prp1: paddr,
            cdw10: low,
            cdw11: high,
            cdw12,
            ..Default::default()
        }
    }

    pub fn nvm_cmd_write(nsid: u32, paddr: u64, starting_lba: u64, blk_num: u16) -> Self {
        let cdw0 = Self::cdw0_from_opcode(command::Opcode::NVM_WRITE);
        let low = (starting_lba & 0xFFFFFFFF) as u32;
        let high = (starting_lba >> 32) as u32;
        let cdw12 = blk_num as u32;

        CommandSet {
            nsid,
            cdw0,
            prp1: paddr,
            cdw10: low,
            cdw11: high,
            cdw12,
            ..Default::default()
        }
    }
}

impl Submission for CommandSet {
    fn to_submission(self) -> NvmeSubmission {
        unsafe { mem::transmute(self) }
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
struct NvmeCompletion {
    pub result: u64,
    pub sq_head: u16,
    pub sq_id: u16,
    pub command_id: u16,
    pub status: CompletionStatus,
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, Default)]
struct CompletionStatus(pub u16);

impl CompletionStatus {
    pub fn phase(&self) -> bool {
        self.0 & 1 > 0
    }

    fn is_success(&self) -> bool {
        self.0 & (1 << 1) == 0
    }

    // pub fn do_not_retry(&self) -> bool {
    //     self.0 & (1 << 15) > 0
    // }
}

pub struct NvmeQueue {
    pub qid: u32,
    pub sq: SubmitQueue,
    pub cq: CompleteQueue,
    pub reg: NonNull<NvmeReg>,
}

impl NvmeQueue {
    pub fn new(
        qid: u32,
        reg: NonNull<NvmeReg>,
        dma: &DeviceDma,
        page_size: usize,
        sq: usize,
        cq: usize,
    ) -> Result<Self> {
        let submit_queue = SubmitQueue::new(dma, sq, page_size)?;
        let complete_queue = CompleteQueue::new(dma, cq, page_size)?;

        Ok(NvmeQueue {
            sq: submit_queue,
            cq: complete_queue,
            qid,
            reg,
        })
    }

    fn reg(&self) -> &NvmeReg {
        unsafe { self.reg.as_ref() }
    }

    fn submit_admin_data(&mut self, data: CommandSet) {
        let tail = self.sq.submit(data);
        self.reg().write_sq_y_tail_doolbell(self.qid as _, tail);
    }

    pub fn command_sync(&mut self, data: CommandSet) -> Result<()> {
        self.submit_admin_data(data);
        let complete = self.cq.spin_for_complete();

        self.reg()
            .write_cq_y_head_doolbell(self.qid as _, self.cq.head);

        if complete.status.is_success() {
            Ok(())
        } else {
            debug!(
                "command failed: status {:#x}, result {:#x}",
                complete.status.0, complete.result
            );
            Err(Error::Unknown("send command failed"))
        }
    }
}

pub struct SubmitQueue {
    queue: CoherentArray<NvmeSubmission>,
    tail: u32,
}

impl SubmitQueue {
    fn new(dma: &DeviceDma, queue_size: usize, page_size: usize) -> Result<Self> {
        let queue = dma.coherent_array_zero_with_align(queue_size, page_size)?;
        Ok(SubmitQueue { queue, tail: 0 })
    }

    // returns the submission queue tail
    pub fn submit(&mut self, data: impl Submission) -> u32 {
        self.queue.set(self.tail as usize, data.to_submission());

        self.tail += 1;
        if self.tail >= self.len() as u32 {
            self.tail = 0;
        }
        self.tail
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn bus_addr(&self) -> u64 {
        self.queue.dma_addr().as_u64()
    }
}

pub struct CompleteQueue {
    queue: CoherentArray<NvmeCompletion>,
    head: u32,
    phase: bool,
}

impl CompleteQueue {
    fn new(dma: &DeviceDma, queue_size: usize, page_size: usize) -> Result<Self> {
        let queue = dma.coherent_array_zero_with_align(queue_size, page_size)?;
        Ok(CompleteQueue {
            queue,
            head: 0,
            phase: false,
        })
    }

    // check if there is completed command in completion queue
    fn complete(&self) -> Option<NvmeCompletion> {
        let cqe = self.queue.read(self.head as _)?;

        let complete = cqe.status.phase() != self.phase;

        if complete { Some(cqe) } else { None }
    }

    fn spin_for_complete(&mut self) -> NvmeCompletion {
        loop {
            if let Some(e) = self.complete() {
                let next_head = self.head + 1;
                if next_head >= self.queue.len() as u32 {
                    self.head = 0;
                    self.phase = !self.phase;
                } else {
                    self.head = next_head;
                }

                return e;
            }
            spin_loop();
        }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn bus_addr(&self) -> u64 {
        self.queue.dma_addr().as_u64()
    }
}
