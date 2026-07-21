use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    mem,
    ptr::{NonNull, addr_of},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use dma_api::{CoherentArray, DeviceDma};
use mbarrier::{mb, rmb, wmb};
use tock_registers::register_bitfields;

use crate::{
    command::{self, Feature},
    err::*,
    nvme::NvmeMmioLease,
    registers::NvmeReg,
};

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
    pub const fn command_id(&self) -> u16 {
        (self.cdw0 >> 16) as u16
    }

    pub fn cdw0_from_opcode_with_cid(opcode: command::Opcode, cid: u16) -> u32 {
        (CommandDword0::Opcode.val(opcode.as_u32()) + CommandDword0::CommandId.val(cid as u32))
            .value
    }

    pub(crate) fn set_features_with_cid(feature: Feature, cid: u16) -> Self {
        let cdw0 = Self::cdw0_from_opcode_with_cid(command::Opcode::SET_FEATURES, cid);

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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_io_completion_queue_with_cid(
        qid: u32,
        size: u32,
        paddr: u64,
        physically_contiguous: bool,
        interrupts_enabled: bool,
        interrupt_vector: u32,
        cid: u16,
    ) -> CommandSet {
        let cdw0 = Self::cdw0_from_opcode_with_cid(command::Opcode::CREATE_IO_CQ, cid);
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_io_submission_queue_with_cid(
        qid: u32,
        size: u32,
        paddr: u64,
        physically_contiguous: bool,
        priority: u32,
        cqid: u32,
        nvm_set_id: u16,
        cid: u16,
    ) -> CommandSet {
        let cdw0 = Self::cdw0_from_opcode_with_cid(command::Opcode::CREATE_IO_SQ, cid);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompletionCursor {
    head: u32,
    phase: bool,
}

pub(crate) struct OwnerCompletionBatch {
    pub(crate) completed: usize,
    pub(crate) owner_rerun: bool,
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

pub struct NvmeQueue {
    pub qid: u32,
    sq: UnsafeCell<SubmitQueue>,
    cq: UnsafeCell<CompleteQueue>,
    completion_probe: NvmeCompletionProbe,
    reg: NonNull<NvmeReg>,
    _mmio_lease: NvmeMmioLease,
}

// SAFETY: An `NvmeQueue` is advanced by exactly one CPU-pinned maintenance
// owner after creation. Moving the not-yet-published queue does not create an
// alias; every live operation remains serialized by that owner.
unsafe impl Send for NvmeQueue {}

// SAFETY: SQ and CQ storage live in disjoint `UnsafeCell`s. The maintenance
// owner is the sole submitter and completion consumer; lifecycle reset is only
// allowed after the same owner has drained IRQ actions and proved DMA quiesced.
// Hard IRQ owns only separate source-mask and read-only CQ-phase capabilities.
unsafe impl Sync for NvmeQueue {}

impl NvmeQueue {
    pub(crate) fn new(
        qid: u32,
        reg: NonNull<NvmeReg>,
        dma: &DeviceDma,
        page_size: usize,
        sq: usize,
        cq: usize,
        mmio_lease: NvmeMmioLease,
    ) -> Result<Self> {
        let submit_queue = SubmitQueue::new(dma, sq, page_size)?;
        let complete_queue = CompleteQueue::new(dma, cq, page_size)?;
        let completion_probe = complete_queue.probe();

        Ok(NvmeQueue {
            sq: UnsafeCell::new(submit_queue),
            cq: UnsafeCell::new(complete_queue),
            completion_probe,
            qid,
            reg,
            _mmio_lease: mmio_lease,
        })
    }

    fn reg(&self) -> &NvmeReg {
        unsafe { self.reg.as_ref() }
    }

    fn with_sq<R>(&self, f: impl FnOnce(&mut SubmitQueue) -> R) -> R {
        let sq = unsafe { &mut *self.sq.get() };
        f(sq)
    }

    fn with_cq<R>(&self, f: impl FnOnce(&mut CompleteQueue) -> R) -> R {
        let cq = unsafe { &mut *self.cq.get() };
        f(cq)
    }

    fn submit_admin_data(&self, data: CommandSet) {
        let tail = self.with_sq(|sq| sq.submit(data));
        wmb();
        self.reg().write_sq_y_tail_doolbell(self.qid as _, tail);
    }

    pub(crate) fn submit_io_data(&self, data: CommandSet) {
        self.submit_admin_data(data);
    }

    pub(crate) fn submit_admin_command(&self, data: CommandSet) {
        self.submit_admin_data(data);
    }

    /// Consumes one completion in the CPU-pinned maintenance owner.
    ///
    /// The caller may invoke this only while servicing an acknowledged source
    /// event whose exact device vector remains masked.
    pub(crate) fn take_owner_completion(&self) -> Option<NvmeCompletion> {
        let mut completion = None;
        let _ = self.drain_owner_completions(1, |entry| completion = Some(entry));
        completion
    }

    /// Drains one bounded owner batch and publishes its final CQ head once.
    pub(crate) fn drain_owner_completions(
        &self,
        budget: usize,
        mut consume: impl FnMut(NvmeCompletion),
    ) -> OwnerCompletionBatch {
        let mut owner_rerun = false;
        let completed = self.with_cq(|cq| {
            drain_completion_batch(
                budget,
                || {
                    let completion = cq.take_complete()?;
                    Some((completion, cq.cursor()))
                },
                &mut consume,
                |cursor| {
                    // The controller may reuse every retired CQ slot after
                    // observing this head, so all CQE copies must complete
                    // before the single batched MMIO publication.
                    mb();
                    self.reg()
                        .write_cq_y_head_doolbell(self.qid as _, cursor.head);
                    owner_rerun = self.completion_probe.finish_owner_batch(cursor);
                },
            )
        });
        OwnerCompletionBatch {
            completed,
            owner_rerun,
        }
    }

    /// Creates a read-only phase probe for the IRQ endpoint.
    ///
    /// The returned probe never consumes a CQE or changes the owner cursor.
    /// The probe retains the CQ DMA allocation independently of this queue.
    pub(crate) fn completion_probe(&self) -> NvmeCompletionProbe {
        self.completion_probe.clone()
    }

    pub(crate) fn depth(&self) -> usize {
        self.sq_len().min(self.cq_len())
    }

    /// Resets retained queue memory after CC.RDY reached zero.
    ///
    /// # Safety
    ///
    /// The caller must have stopped controller DMA and excluded every IRQ and
    /// maintenance-owner queue consumer before invoking this method.
    pub(crate) unsafe fn reset_after_controller_disable(&self) {
        self.with_sq(SubmitQueue::reset);
        self.with_cq(CompleteQueue::reset);
        wmb();
    }

    pub(crate) fn sq_len(&self) -> usize {
        unsafe { &*self.sq.get() }.len()
    }

    pub(crate) fn cq_len(&self) -> usize {
        unsafe { &*self.cq.get() }.len()
    }

    pub(crate) fn sq_bus_addr(&self) -> u64 {
        unsafe { &*self.sq.get() }.bus_addr()
    }

    pub(crate) fn cq_bus_addr(&self) -> u64 {
        unsafe { &*self.cq.get() }.bus_addr()
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

    fn reset(&mut self) {
        // The controller cannot inspect an SQ entry until the new tail is
        // published. Resetting the producer index is sufficient and avoids a
        // potentially large memory sweep in a bounded lifecycle transition.
        self.tail = 0;
    }
}

pub struct CompleteQueue {
    storage: Arc<CompletionQueueStorage>,
    head: u32,
    phase: bool,
    published_cursor: Arc<PublishedCompletionCursor>,
}

/// Read-only CQ phase capability owned by an IRQ endpoint.
///
/// The maintenance owner publishes its consumer cursor atomically. The probe
/// reads only the phase bit at that cursor, so it can distinguish a shared
/// INTx peer from an NVMe completion without consuming queue state.
#[derive(Clone)]
pub(crate) struct NvmeCompletionProbe {
    backing: CompletionProbeBacking,
    cursor: Arc<PublishedCompletionCursor>,
}

#[derive(Clone)]
enum CompletionProbeBacking {
    Dma(Arc<CompletionQueueStorage>),
    #[cfg(test)]
    Test {
        entries: NonNull<NvmeCompletion>,
        len: usize,
    },
}

struct CompletionQueueStorage(UnsafeCell<CoherentArray<NvmeCompletion>>);

struct PublishedCompletionCursor {
    cursor: AtomicU64,
    irq_claimed: AtomicBool,
}

impl CompleteQueue {
    fn new(dma: &DeviceDma, queue_size: usize, page_size: usize) -> Result<Self> {
        let storage = Arc::new(CompletionQueueStorage(UnsafeCell::new(
            dma.coherent_array_zero_with_align(queue_size, page_size)?,
        )));
        Ok(CompleteQueue {
            storage,
            head: 0,
            phase: false,
            published_cursor: Arc::new(PublishedCompletionCursor::new(0, false)),
        })
    }

    // check if there is completed command in completion queue
    fn complete(&self) -> Option<NvmeCompletion> {
        read_completed_entry(
            self.phase,
            || self.read_head_status(),
            || self.read_head_entry(),
            rmb,
        )
    }

    fn read_head_status(&self) -> Option<CompletionStatus> {
        let index = usize::try_from(self.head).ok()?;
        self.storage.read_status(index)
    }

    fn read_head_entry(&self) -> Option<NvmeCompletion> {
        let index = usize::try_from(self.head).ok()?;
        self.storage.read_entry(index)
    }

    fn take_complete(&mut self) -> Option<NvmeCompletion> {
        let complete = self.complete()?;
        let next_head = self.head + 1;
        if next_head >= self.storage.len() as u32 {
            self.head = 0;
            self.phase = !self.phase;
        } else {
            self.head = next_head;
        }

        Some(complete)
    }

    fn cursor(&self) -> CompletionCursor {
        CompletionCursor::new(self.head, self.phase)
    }

    fn probe(&self) -> NvmeCompletionProbe {
        NvmeCompletionProbe {
            backing: CompletionProbeBacking::Dma(Arc::clone(&self.storage)),
            cursor: Arc::clone(&self.published_cursor),
        }
    }

    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn bus_addr(&self) -> u64 {
        self.storage.bus_addr()
    }

    fn reset(&mut self) {
        unsafe {
            // SAFETY: callers reset a CQ only after controller DMA stopped and
            // every IRQ action plus maintenance owner was drained.
            self.storage.clear_after_quiesce();
        }
        self.head = 0;
        self.phase = false;
        self.published_cursor
            .reset_after_quiesce(CompletionCursor::new(self.head, self.phase));
    }
}

impl CompletionCursor {
    const fn new(head: u32, phase: bool) -> Self {
        Self { head, phase }
    }
}

fn drain_completion_batch<T>(
    budget: usize,
    mut next: impl FnMut() -> Option<(T, CompletionCursor)>,
    mut consume: impl FnMut(T),
    publish: impl FnOnce(CompletionCursor),
) -> usize {
    let mut completed = 0;
    let mut final_cursor = None;
    while completed < budget {
        let Some((completion, cursor)) = next() else {
            break;
        };
        consume(completion);
        final_cursor = Some(cursor);
        completed += 1;
    }
    if let Some(cursor) = final_cursor {
        publish(cursor);
    }
    completed
}

fn read_completed_entry(
    consumer_phase: bool,
    read_status: impl FnOnce() -> Option<CompletionStatus>,
    read_entry: impl FnOnce() -> Option<NvmeCompletion>,
    read_barrier: impl FnOnce(),
) -> Option<NvmeCompletion> {
    let observed_phase = read_status()?.phase();
    if observed_phase == consumer_phase {
        return None;
    }
    read_barrier();
    read_entry()
}

fn read_completion_status(
    entries: NonNull<NvmeCompletion>,
    len: usize,
    index: usize,
) -> Option<CompletionStatus> {
    if index >= len {
        return None;
    }
    let entry = unsafe {
        // SAFETY: `index` is within the retained coherent CQ allocation.
        entries.as_ptr().add(index)
    };
    Some(unsafe {
        // SAFETY: the controller publishes the phase bit through coherent DMA
        // and may change it independently of Rust. A volatile field read is
        // therefore required and does not create a reference to DMA memory.
        addr_of!((*entry).status).read_volatile()
    })
}

impl NvmeCompletionProbe {
    /// Claims one pending CQ phase observation for IRQ publication.
    ///
    /// Repeated IRQ callbacks coalesce until the owner advances the cursor.
    pub(crate) fn try_claim_pending(&self) -> bool {
        if self
            .cursor
            .irq_claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        let (head, consumer_phase) = self.cursor.load();
        let pending = self
            .backing
            .read_status(head)
            .is_some_and(|status| status.phase() != consumer_phase);
        if !pending {
            self.cursor.irq_claimed.store(false, Ordering::Release);
        }
        pending
    }

    /// Publishes owner progress, releases the old IRQ claim, and closes the
    /// edge-loss window by rechecking the newly exposed CQ head.
    ///
    /// A `true` result means the maintenance owner must run another local
    /// service batch. The owner reclaims the CQ fact when possible; if an IRQ
    /// callback won the claim race, both paths may coalesce harmlessly.
    fn finish_owner_batch(&self, cursor: CompletionCursor) -> bool {
        self.cursor.publish(cursor);
        self.cursor.irq_claimed.store(false, Ordering::Release);
        let pending = self
            .backing
            .read_status(cursor.head as usize)
            .is_some_and(|status| status.phase() != cursor.phase);
        if pending {
            let _ = self.cursor.irq_claimed.compare_exchange(
                false,
                true,
                Ordering::Acquire,
                Ordering::Relaxed,
            );
        }
        pending
    }

    #[cfg(test)]
    /// Creates a probe over test-owned CQ entries.
    ///
    /// # Safety
    ///
    /// `entries` must outlive the returned probe and must not be moved while
    /// the probe is used. Concurrent mutation must follow the same coherent
    /// DMA publication rules as a real NVMe completion queue.
    pub(crate) unsafe fn from_test_entries(
        entries: &mut [NvmeCompletion],
        head: usize,
        consumer_phase: bool,
    ) -> Self {
        let entries_ptr = NonNull::new(entries.as_mut_ptr())
            .expect("a test completion queue must have at least one entry");
        Self {
            backing: CompletionProbeBacking::Test {
                entries: entries_ptr,
                len: entries.len(),
            },
            cursor: Arc::new(PublishedCompletionCursor::new(head, consumer_phase)),
        }
    }
}

impl CompletionProbeBacking {
    fn read_status(&self, index: usize) -> Option<CompletionStatus> {
        match self {
            Self::Dma(storage) => storage.read_status(index),
            #[cfg(test)]
            Self::Test { entries, len } => read_completion_status(*entries, *len, index),
        }
    }
}

impl CompletionQueueStorage {
    fn array(&self) -> &CoherentArray<NvmeCompletion> {
        unsafe {
            // SAFETY: normal owner and IRQ access are read-only. The only
            // mutable access is proof-gated reset after both have drained.
            &*self.0.get()
        }
    }

    fn read_status(&self, index: usize) -> Option<CompletionStatus> {
        let queue = self.array();
        read_completion_status(queue.as_ptr(), queue.len(), index)
    }

    fn read_entry(&self, index: usize) -> Option<NvmeCompletion> {
        let queue = self.array();
        if index >= queue.len() {
            return None;
        }
        Some(unsafe {
            // SAFETY: the checked index addresses retained coherent storage.
            // The controller publishes through DMA, so the CPU copies the CQE
            // with a volatile load after the caller's read barrier.
            queue.as_ptr().add(index).read_volatile()
        })
    }

    fn len(&self) -> usize {
        self.array().len()
    }

    fn bus_addr(&self) -> u64 {
        self.array().dma_addr().as_u64()
    }

    unsafe fn clear_after_quiesce(&self) {
        let queue = unsafe {
            // SAFETY: the caller proves no device, IRQ, probe, or owner access
            // can overlap this exclusive mutation of retained DMA storage.
            &mut *self.0.get()
        };
        for index in 0..queue.len() {
            queue.set_cpu(index, NvmeCompletion::default());
        }
    }
}

impl PublishedCompletionCursor {
    fn new(head: usize, phase: bool) -> Self {
        Self {
            cursor: AtomicU64::new(Self::encode(head, phase)),
            irq_claimed: AtomicBool::new(false),
        }
    }

    fn publish(&self, cursor: CompletionCursor) {
        self.cursor.store(
            Self::encode(cursor.head as usize, cursor.phase),
            Ordering::Release,
        );
    }

    fn reset_after_quiesce(&self, cursor: CompletionCursor) {
        self.publish(cursor);
        self.irq_claimed.store(false, Ordering::Release);
    }

    fn load(&self) -> (usize, bool) {
        let cursor = self.cursor.load(Ordering::Acquire);
        (cursor as u32 as usize, cursor >> u32::BITS != 0)
    }

    fn encode(head: usize, phase: bool) -> u64 {
        debug_assert!(u32::try_from(head).is_ok());
        head as u32 as u64 | ((phase as u64) << u32::BITS)
    }
}

// SAFETY: production probes retain their coherent CQ allocation. Test probes
// have an explicit unsafe lifetime contract. Both read DMA memory only through
// a checked volatile phase-field load and share only an atomic cursor.
unsafe impl Send for NvmeCompletionProbe {}

// SAFETY: `try_claim_pending` does not mutate the CQ or owner cursor. Concurrent
// callers observe an atomically published cursor and volatile DMA phase field.
unsafe impl Sync for NvmeCompletionProbe {}

// SAFETY: normal access to the coherent allocation is read-only. Mutation is
// confined to `clear_after_quiesce`, whose unsafe contract excludes device,
// IRQ, probe, and maintenance-owner access.
unsafe impl Send for CompletionQueueStorage {}

// SAFETY: see the `Send` implementation; all shared live operations perform
// checked volatile reads and the sole mutation requires global quiescence.
unsafe impl Sync for CompletionQueueStorage {}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};

    use super::{
        CompletionCursor, CompletionStatus, NvmeCompletion, NvmeCompletionProbe,
        drain_completion_batch, read_completed_entry,
    };

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

    #[test]
    fn completion_phase_orders_cqe_fields_after_the_read_barrier() {
        let trace = RefCell::new(Vec::new());
        let reads = Cell::new(0);
        let completion = NvmeCompletion {
            status: CompletionStatus(1),
            ..NvmeCompletion::default()
        };

        let observed = read_completed_entry(
            false,
            || {
                reads.set(reads.get() + 1);
                trace.borrow_mut().push("phase");
                Some(completion.status)
            },
            || {
                reads.set(reads.get() + 1);
                trace.borrow_mut().push("entry");
                Some(completion)
            },
            || trace.borrow_mut().push("barrier"),
        );

        assert!(observed.is_some());
        assert_eq!(reads.get(), 2);
        assert_eq!(&*trace.borrow(), &["phase", "barrier", "entry"]);
    }

    #[test]
    fn bounded_completion_batch_publishes_only_the_final_head() {
        let mut source = [
            (11, CompletionCursor::new(1, false)),
            (22, CompletionCursor::new(2, false)),
            (33, CompletionCursor::new(3, false)),
        ]
        .into_iter();
        let mut consumed = Vec::new();
        let mut published = Vec::new();

        let completed = drain_completion_batch(
            64,
            || source.next(),
            |value| consumed.push(value),
            |cursor| published.push(cursor),
        );

        assert_eq!(completed, 3);
        assert_eq!(consumed, [11, 22, 33]);
        assert_eq!(published, [CompletionCursor::new(3, false)]);
    }

    #[test]
    fn owner_handoff_reclaims_a_cqe_whose_irq_edge_was_coalesced() {
        let mut entries = [
            NvmeCompletion {
                status: CompletionStatus(1),
                ..NvmeCompletion::default()
            },
            NvmeCompletion::default(),
        ];
        let probe = unsafe {
            // SAFETY: the array remains pinned on this test stack for every
            // phase observation, and mutation models coherent device writes.
            NvmeCompletionProbe::from_test_entries(&mut entries, 0, false)
        };
        assert!(probe.try_claim_pending());

        unsafe {
            // SAFETY: this volatile write models a coherent device DMA update
            // to the pinned test CQ while the probe retains a raw phase view.
            core::ptr::write_volatile(&mut entries[1].status, CompletionStatus(1));
        }
        assert!(
            !probe.try_claim_pending(),
            "the new IRQ edge must coalesce while the old owner claim is live"
        );

        assert!(probe.finish_owner_batch(CompletionCursor::new(1, false)));
        assert!(
            !probe.try_claim_pending(),
            "the owner-local rerun must retain the CQ claim until it drains the new fact"
        );
    }
}
