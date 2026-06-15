use core::{
    hint::spin_loop,
    mem,
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

use dma_api::{CoherentArray, DeviceDma};
use log::debug;
use mbarrier::{rmb, wmb};
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
        Self::cdw0_from_opcode_with_cid(opcode, next_id() as u16)
    }

    pub fn cdw0_from_opcode_with_cid(opcode: command::Opcode, cid: u16) -> u32 {
        (CommandDword0::Opcode.val(opcode.as_u32()) + CommandDword0::CommandId.val(cid as u32))
            .value
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

    pub fn nvm_cmd_read(nsid: u32, paddr: u64, starting_lba: u64, blk_num: u32) -> Self {
        Self::nvm_cmd_read_with_cid(nsid, paddr, 0, starting_lba, blk_num, next_id() as u16)
    }

    pub fn nvm_cmd_read_with_cid(
        nsid: u32,
        prp1: u64,
        prp2: u64,
        starting_lba: u64,
        block_count: u32,
        cid: u16,
    ) -> Self {
        let cdw0 = Self::cdw0_from_opcode_with_cid(command::Opcode::NVM_READ, cid);
        let low = (starting_lba & 0xFFFFFFFF) as u32;
        let high = (starting_lba >> 32) as u32;
        let cdw12 = block_count.saturating_sub(1);

        CommandSet {
            nsid,
            cdw0,
            prp1,
            prp2,
            cdw10: low,
            cdw11: high,
            cdw12,
            ..Default::default()
        }
    }

    pub fn nvm_cmd_write(nsid: u32, paddr: u64, starting_lba: u64, blk_num: u32) -> Self {
        Self::nvm_cmd_write_with_cid(nsid, paddr, 0, starting_lba, blk_num, next_id() as u16)
    }

    pub fn nvm_cmd_write_with_cid(
        nsid: u32,
        prp1: u64,
        prp2: u64,
        starting_lba: u64,
        block_count: u32,
        cid: u16,
    ) -> Self {
        let cdw0 = Self::cdw0_from_opcode_with_cid(command::Opcode::NVM_WRITE, cid);
        let low = (starting_lba & 0xFFFFFFFF) as u32;
        let high = (starting_lba >> 32) as u32;
        let cdw12 = block_count.saturating_sub(1);

        CommandSet {
            nsid,
            cdw0,
            prp1,
            prp2,
            cdw10: low,
            cdw11: high,
            cdw12,
            ..Default::default()
        }
    }

    pub fn nvm_cmd_flush_with_cid(nsid: u32, cid: u16) -> Self {
        let cdw0 = Self::cdw0_from_opcode_with_cid(command::Opcode::NVM_FLUSH, cid);

        CommandSet {
            nsid,
            cdw0,
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
pub(crate) struct NvmeCompletion {
    pub result: u64,
    pub sq_head: u16,
    pub sq_id: u16,
    pub command_id: u16,
    pub status: CompletionStatus,
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, Default)]
pub(crate) struct CompletionStatus(pub u16);

impl CompletionStatus {
    pub fn phase(&self) -> bool {
        self.0 & 1 > 0
    }

    fn status_field(&self) -> u16 {
        (self.0 >> 1) & 0x7ff
    }

    pub(crate) fn is_success(&self) -> bool {
        self.status_field() == 0
    }

    // pub fn do_not_retry(&self) -> bool {
    //     self.0 & (1 << 15) > 0
    // }
}

#[cfg(test)]
mod tests {
    use super::CompletionStatus;

    #[test]
    fn completion_status_ignores_phase_for_success() {
        assert!(CompletionStatus(0).is_success());
        assert!(CompletionStatus(1).is_success());
    }

    #[test]
    fn completion_status_checks_full_status_field() {
        assert!(!CompletionStatus(1 | (1 << 2)).is_success());
        assert!(!CompletionStatus(1 | (1 << 9)).is_success());
        assert!(!CompletionStatus(1 | (1 << 11)).is_success());
    }
}

pub struct NvmeQueue {
    pub qid: u32,
    pub sq: SubmitQueue,
    pub cq: CompleteQueue,
    pub reg: NonNull<NvmeReg>,
}

// SAFETY: An `NvmeQueue` is owned by exactly one RDIF queue after creation.
// Moving that owner between threads does not create aliasing; register access
// still happens through `&mut self` queue methods.
unsafe impl Send for NvmeQueue {}

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
        wmb();
        self.reg().write_sq_y_tail_doolbell(self.qid as _, tail);
    }

    pub(crate) fn submit_io_data(&mut self, data: CommandSet) {
        self.submit_admin_data(data);
    }

    pub(crate) fn poll_completion(&mut self) -> Option<NvmeCompletion> {
        let complete = self.cq.take_complete()?;
        wmb();
        self.reg()
            .write_cq_y_head_doolbell(self.qid as _, self.cq.head);
        Some(complete)
    }

    pub(crate) fn depth(&self) -> usize {
        self.sq.len().min(self.cq.len())
    }

    pub fn command_sync(&mut self, data: CommandSet) -> Result<()> {
        self.submit_admin_data(data);
        let complete = self.cq.spin_for_complete();
        wmb();
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
        self.queue.set_cpu(self.tail as usize, data.to_submission());

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
        rmb();
        let cqe = self.queue.read_cpu(self.head as _)?;

        let complete = cqe.status.phase() != self.phase;

        if complete { Some(cqe) } else { None }
    }

    fn spin_for_complete(&mut self) -> NvmeCompletion {
        loop {
            if let Some(e) = self.take_complete() {
                return e;
            }
            spin_loop();
        }
    }

    fn take_complete(&mut self) -> Option<NvmeCompletion> {
        let complete = self.complete()?;
        let next_head = self.head + 1;
        if next_head >= self.queue.len() as u32 {
            self.head = 0;
            self.phase = !self.phase;
        } else {
            self.head = next_head;
        }

        Some(complete)
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn bus_addr(&self) -> u64 {
        self.queue.dma_addr().as_u64()
    }
}
