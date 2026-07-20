//! Controller discovery, initialization, IRQ routing, and recovery ownership.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    any::Any,
    cell::UnsafeCell,
    num::NonZeroUsize,
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use dma_api::DmaOp;
use mmio_api::{MmioAddr, MmioOp};
use rdif_block::{
    BlkError, BlockIrqSource, ControllerInitEndpoint, DeviceInfo, DriverGeneric, IdList, InitError,
    InitInput, InitPoll, InitialController, Interface, InterruptLifecycle, IrqSourceList,
    LifecycleEndpoint, QueueHandle, QueueLimits, RecoveryCause,
};

use super::{
    NvmeBlockQueue, NvmeIrqState, NvmeQueueCore, NvmeQueueReinitializeInfo, alloc_prp_lists,
    device_info, irq_sources_from_queue_bits, limits, new_initial_irq_source, new_queue_irq_source,
    queue_interrupt_sources, source_queue_bits, vector_for_queue,
};
use crate::{
    Config, Namespace,
    command::Feature,
    err::{Error, Result as NvmeResult},
    initialization::{InitialAdminCommand, InitialHardware, NvmeInitialization},
    lifecycle::{
        AdminCommand, AdminCompletion, LifecycleHardware, NvmeLifecycle, queue_count_supported,
    },
    nvme::Nvme,
    queue::{CommandSet, NvmeQueue as HardwareQueue},
};

const DEFAULT_QUEUE_DEPTH: usize = 64;

pub(super) fn effective_queue_depth(
    requested_depth: usize,
    hardware_queue_entries: usize,
) -> Option<NonZeroUsize> {
    let usable_entries = hardware_queue_entries.checked_sub(1)?;
    NonZeroUsize::new(requested_depth.max(1).min(usable_entries))
}
const ADMIN_COMMAND_ID: u16 = 0;

struct NvmeBlockInner {
    nvme: Nvme,
    namespace: Option<Namespace>,
}

pub struct NvmeBlockDriver {
    name: &'static str,
    inner: Arc<NvmeBlockOwner>,
    initialization: NvmeInitialization,
    lifecycle: NvmeLifecycle,
    queue_depth: usize,
}

pub(super) struct NvmeBlockOwner {
    inner: UnsafeCell<NvmeBlockInner>,
    irq: Arc<NvmeIrqState>,
    queues: UnsafeCell<Vec<Arc<NvmeQueueCore>>>,
    next_queue_id: AtomicUsize,
    created_queue_bits: AtomicU64,
    irq_supported: bool,
    msix_interrupts: bool,
    interrupt_vectors: Vec<u16>,
    admin_queue: Arc<HardwareQueue>,
    admin_command_pending: AtomicBool,
}

impl NvmeBlockDriver {
    /// Discovers an NVMe controller without issuing a hardware command.
    ///
    /// The returned interface exposes [`ControllerInitEndpoint::Pending`]. An
    /// OS runtime must bind its initialization IRQ endpoint before the first
    /// state-machine poll. Capacity and I/O queues remain unavailable until
    /// that state machine reaches `Ready`.
    pub fn discover(
        name: &'static str,
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
        config: Config,
    ) -> NvmeResult<Self> {
        Self::discover_with_queue_depth(
            name,
            bar_addr,
            bar_size,
            dma_mask,
            dma_op,
            mmio_op,
            config,
            DEFAULT_QUEUE_DEPTH,
        )
    }

    /// Discovers an NVMe controller with a runtime request-depth cap.
    #[allow(clippy::too_many_arguments)]
    pub fn discover_with_queue_depth(
        name: &'static str,
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
        config: Config,
        queue_depth: usize,
    ) -> NvmeResult<Self> {
        let nvme = Nvme::discover(bar_addr, bar_size, dma_mask, dma_op, mmio_op, config)?;
        let queue_depth = effective_queue_depth(queue_depth, nvme.io_queue_entries()).ok_or(
            Error::Unknown("NVMe I/O queues cannot retain one in-flight request"),
        )?;
        let irq_supported = nvme.io_queue_interrupts_enabled();
        let msix_interrupts = nvme.msix_interrupts_enabled();
        let interrupt_vectors = nvme.interrupt_vectors().to_vec();
        let admin_queue = nvme.admin_queue();
        let interrupt_port = unsafe {
            // SAFETY: the port and `nvme` move into the same Arc owner. Every
            // endpoint/control reference retains that owner and therefore the
            // BAR mapping for the complete port lifetime.
            nvme.interrupt_port()
        };
        let irq = Arc::new(NvmeIrqState::new(
            interrupt_port,
            &interrupt_vectors,
            msix_interrupts,
        ));
        let inner = Arc::new(NvmeBlockOwner {
            inner: UnsafeCell::new(NvmeBlockInner {
                nvme,
                namespace: None,
            }),
            irq,
            queues: UnsafeCell::new(Vec::new()),
            next_queue_id: AtomicUsize::new(0),
            created_queue_bits: AtomicU64::new(0),
            irq_supported,
            msix_interrupts,
            interrupt_vectors,
            admin_queue,
            admin_command_pending: AtomicBool::new(false),
        });
        Ok(Self {
            name,
            inner,
            initialization: NvmeInitialization::discovered(),
            lifecycle: NvmeLifecycle::new(),
            queue_depth: queue_depth.get(),
        })
    }

    pub fn namespace_if_ready(&self) -> Option<Namespace> {
        self.inner.with_ref(|inner| inner.namespace)
    }

    pub fn into_interface(self) -> Self {
        self
    }

    fn device_info_for(&self) -> DeviceInfo {
        device_info(self.name, self.ready_namespace())
    }

    fn limits_for(&self) -> QueueLimits {
        let namespace = self.ready_namespace();
        self.inner.with_ref(|inner| {
            limits(
                inner.nvme.dma_mask(),
                inner.nvme.page_size(),
                inner.nvme.max_transfer_bytes(),
                namespace,
                self.queue_depth,
            )
        })
    }

    fn ready_namespace(&self) -> Namespace {
        self.namespace_if_ready()
            .expect("NVMe capacity is unavailable before controller initialization")
    }
}

// SAFETY: RDIF queue ownership gives every I/O queue to one CPU-pinned
// maintenance domain. Hard IRQ holds only the disjoint `NvmeIrqState`, and the
// owner keeps the controller and MMIO mapping alive until all queues close.
unsafe impl Send for NvmeBlockOwner {}

// SAFETY: mutable controller access is serialized by the maintenance owner.
// The queue registry freezes before source registration, and IRQ masking uses
// the disjoint `NvmeIrqState` without borrowing controller or queue state.
unsafe impl Sync for NvmeBlockOwner {}

impl NvmeBlockOwner {
    fn with_ref<R>(&self, f: impl FnOnce(&NvmeBlockInner) -> R) -> R {
        // SAFETY: the owner-side initialization/lifecycle state machines
        // serialize these controller borrows. IRQ endpoints use only the
        // disjoint source-mask capability and frozen routing bitmap.
        let inner = unsafe { &*self.inner.get() };
        f(inner)
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut NvmeBlockInner) -> R) -> R {
        // SAFETY: Interface control operations are serialized by the owning
        // runtime. Published IRQ endpoints cannot borrow controller or queue
        // state because their type owns only `NvmeIrqState`.
        let inner = unsafe { &mut *self.inner.get() };
        f(inner)
    }

    fn register_queue(&self, queue: Arc<NvmeQueueCore>) {
        // SAFETY: `create_queue` requires exclusive Interface access and is
        // rejected after any IRQ source is taken, so the registry cannot be
        // read concurrently while it is being extended.
        let queues = unsafe { &mut *self.queues.get() };
        queues.push(queue);
    }

    pub(super) fn queues(&self) -> &[Arc<NvmeQueueCore>] {
        // SAFETY: taking the first IRQ source freezes queue creation. Endpoint
        // lifetime therefore observes an immutable, owner-retained registry.
        unsafe { &*self.queues.get() }
    }

    pub(super) fn source_queue_bits(&self, source_id: usize, queue_bits: u64) -> u64 {
        source_queue_bits(
            self.msix_interrupts,
            &self.interrupt_vectors,
            source_id,
            queue_bits,
        )
    }

    fn irq_sources_from_queue_bits(&self, queue_bits: u64) -> IrqSourceList {
        irq_sources_from_queue_bits(self.msix_interrupts, &self.interrupt_vectors, queue_bits)
    }

    fn required_io_irq_source_bits(&self) -> u64 {
        let queue_bits = self.created_queue_bits();
        let mut source_bits = 0_u64;
        for queue_id in 0..u64::BITS as usize {
            if queue_bits & (1_u64 << queue_id) == 0 {
                continue;
            }
            if let Some(vector) =
                vector_for_queue(self.msix_interrupts, &self.interrupt_vectors, queue_id)
            {
                source_bits |= 1_u64 << usize::from(vector);
            }
        }
        source_bits
    }

    pub(super) fn controller_cookie(&self) -> usize {
        ptr::from_ref(self).expose_provenance()
    }

    pub(super) fn admin_irq_source_id(&self) -> Option<usize> {
        if !self.irq_supported {
            return None;
        }
        if !self.msix_interrupts || self.interrupt_vectors.contains(&0) {
            Some(0)
        } else {
            None
        }
    }

    fn clear_admin_completion_after_quiesce(&self) -> Result<(), InitError> {
        self.admin_command_pending.store(false, Ordering::Release);
        Ok(())
    }

    fn submit_lifecycle_admin(&self, command: CommandSet) -> Result<u16, InitError> {
        if self
            .admin_command_pending
            .compare_exchange(false, true, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            return Err(InitError::InvalidState);
        }
        let command_id = command.command_id();
        self.admin_queue.submit_admin_command(command);
        Ok(command_id)
    }

    fn take_admin_completion(&self) -> Option<AdminCompletion> {
        if !self.admin_command_pending.load(Ordering::Acquire) {
            return None;
        }
        let completion = self.admin_queue.take_owner_completion()?;
        let completion = AdminCompletion {
            command_id: completion.command_id,
            success: completion.status.is_success(),
            result: completion.result,
        };
        self.admin_command_pending.store(false, Ordering::Release);
        Some(completion)
    }

    fn queue_reinitialize_command(&self, command: AdminCommand) -> Result<CommandSet, InitError> {
        match command {
            AdminCommand::IdentifyController
            | AdminCommand::IdentifyNamespaceList
            | AdminCommand::IdentifyNamespace { .. } => {
                self.with_ref(|inner| inner.nvme.build_reidentify_admin_command(command))
            }
            AdminCommand::SetQueueCount { count } => {
                let queue_count = u32::try_from(count)
                    .ok()
                    .and_then(|count| count.checked_sub(1))
                    .ok_or(InitError::Hardware("invalid NVMe I/O queue count"))?;
                Ok(CommandSet::set_features_with_cid(
                    Feature::NumberOfQueues {
                        nsq: queue_count,
                        ncq: queue_count,
                    },
                    ADMIN_COMMAND_ID,
                ))
            }
            AdminCommand::CreateCompletionQueue { queue_index } => {
                let queue = self.queue_for_reinitialize(queue_index)?;
                let vector =
                    vector_for_queue(self.msix_interrupts, &self.interrupt_vectors, queue_index)
                        .ok_or(InitError::MissingInterrupt)?;
                Ok(CommandSet::create_io_completion_queue_with_cid(
                    queue.qid,
                    u32::try_from(queue.cq_len)
                        .map_err(|_| InitError::Hardware("NVMe completion queue is too large"))?,
                    queue.cq_bus_addr,
                    true,
                    true,
                    u32::from(vector),
                    ADMIN_COMMAND_ID,
                ))
            }
            AdminCommand::CreateSubmissionQueue { queue_index } => {
                let queue = self.queue_for_reinitialize(queue_index)?;
                Ok(CommandSet::create_io_submission_queue_with_cid(
                    queue.qid,
                    u32::try_from(queue.sq_len)
                        .map_err(|_| InitError::Hardware("NVMe submission queue is too large"))?,
                    queue.sq_bus_addr,
                    true,
                    0,
                    queue.qid,
                    0,
                    ADMIN_COMMAND_ID,
                ))
            }
        }
    }

    fn complete_reinitialize_admin(
        &self,
        command: AdminCommand,
        completion: AdminCompletion,
    ) -> Result<Option<AdminCommand>, InitError> {
        let queue_count = self.queues().len();
        match command {
            AdminCommand::IdentifyController => {
                self.with_ref(|inner| inner.nvme.validate_reidentified_controller())?;
                Ok(Some(AdminCommand::SetQueueCount { count: queue_count }))
            }
            AdminCommand::SetQueueCount { count } => {
                if count != queue_count || !queue_count_supported(completion.result, count) {
                    return Err(InitError::Hardware(
                        "NVMe controller did not restore the required queue count",
                    ));
                }
                Ok(Some(AdminCommand::CreateCompletionQueue { queue_index: 0 }))
            }
            AdminCommand::CreateCompletionQueue { queue_index } => {
                Ok(Some(AdminCommand::CreateSubmissionQueue { queue_index }))
            }
            AdminCommand::CreateSubmissionQueue { queue_index } => {
                let next = queue_index.saturating_add(1);
                if next < queue_count {
                    Ok(Some(AdminCommand::CreateCompletionQueue {
                        queue_index: next,
                    }))
                } else {
                    Ok(Some(AdminCommand::IdentifyNamespaceList))
                }
            }
            AdminCommand::IdentifyNamespaceList => {
                let namespace_id =
                    self.with_ref(|inner| inner.nvme.validate_reidentified_namespace_list())?;
                Ok(Some(AdminCommand::IdentifyNamespace { namespace_id }))
            }
            AdminCommand::IdentifyNamespace { namespace_id } => {
                self.with_ref(|inner| inner.nvme.validate_reidentified_namespace(namespace_id))?;
                Ok(None)
            }
        }
    }

    fn queue_for_reinitialize(&self, index: usize) -> Result<NvmeQueueReinitializeInfo, InitError> {
        let core = self
            .queues()
            .get(index)
            .ok_or(InitError::Hardware("missing retained NVMe I/O queue"))?;
        Ok(core.reinitialize_info)
    }

    fn publish_ready_namespace(&self) -> Result<(), InitError> {
        self.with_mut(|inner| {
            let namespace = inner.nvme.namespace_if_ready().ok_or(InitError::Hardware(
                "NVMe initialization produced no namespace",
            ))?;
            inner.namespace = Some(namespace);
            Ok(())
        })
    }

    pub(super) fn created_queue_bits(&self) -> u64 {
        self.created_queue_bits.load(Ordering::Acquire)
    }
}

impl InitialHardware for NvmeBlockOwner {
    fn controller_timeout_ns(&self) -> u64 {
        self.with_ref(|inner| inner.nvme.controller_timeout_ns())
    }

    fn begin_controller_disable(&self) {
        self.with_ref(|inner| inner.nvme.begin_controller_disable());
    }

    fn controller_ready(&self) -> bool {
        self.with_ref(|inner| inner.nvme.controller_ready())
    }

    fn controller_fatal(&self) -> bool {
        self.with_ref(|inner| inner.nvme.controller_fatal())
    }

    fn live_admin_irq_source(&self) -> Option<usize> {
        (self.irq.initial_source_live() && self.irq.delivery_enabled())
            .then(|| self.admin_irq_source_id())
            .flatten()
    }

    unsafe fn prepare_initial_enable(&self) -> Result<(), InitError> {
        self.clear_admin_completion_after_quiesce()?;
        let source_id = self
            .live_admin_irq_source()
            .ok_or(InitError::MissingInterrupt)?;
        self.with_ref(|inner| {
            // SAFETY: InitialHardware requires RDY=0 and a live, drained init
            // IRQ action before this transition programs retained queue DMA.
            unsafe { inner.nvme.prepare_initial_enable() };
        });
        // The OS action is already live, but discovery kept the device source
        // masked. Unmask only after stale admin CQ state was reset and before
        // any admin command can be submitted.
        self.irq.unmask_for_activation(source_id)?;
        Ok(())
    }

    fn submit_initial_admin(&self, command: InitialAdminCommand) -> Result<u16, InitError> {
        let command = self.with_ref(|inner| inner.nvme.build_initial_admin_command(command))?;
        self.submit_lifecycle_admin(command)
    }

    fn take_admin_completion(&self) -> Option<AdminCompletion> {
        NvmeBlockOwner::take_admin_completion(self)
    }

    fn complete_initial_admin(
        &self,
        command: InitialAdminCommand,
        completion: AdminCompletion,
    ) -> Result<Option<InitialAdminCommand>, InitError> {
        self.with_mut(|inner| inner.nvme.complete_initial_admin(command, completion))
    }

    fn publish_ready(&self) -> Result<(), InitError> {
        self.publish_ready_namespace()
    }
}

impl LifecycleHardware for NvmeBlockOwner {
    fn controller_cookie(&self) -> usize {
        NvmeBlockOwner::controller_cookie(self)
    }

    fn controller_timeout_ns(&self) -> u64 {
        self.with_ref(|inner| inner.nvme.controller_timeout_ns())
    }

    fn begin_controller_disable(&self) {
        self.with_ref(|inner| inner.nvme.begin_controller_disable());
    }

    fn controller_ready(&self) -> bool {
        self.with_ref(|inner| inner.nvme.controller_ready())
    }

    fn controller_fatal(&self) -> bool {
        self.with_ref(|inner| inner.nvme.controller_fatal())
    }

    unsafe fn prepare_reinitialize(&self) -> Result<(), InitError> {
        self.clear_admin_completion_after_quiesce()?;
        for queue in self.queues() {
            // SAFETY: this trait method requires the exact controller proof
            // and drained hctx/IRQ access described by LifecycleHardware.
            unsafe { queue.reset_after_quiesce()? };
        }
        self.with_ref(|inner| {
            // SAFETY: admin queue memory is covered by the same controller
            // quiescence and IRQ-drain preconditions as the I/O queues.
            unsafe { inner.nvme.prepare_controller_reinitialize() };
        });
        Ok(())
    }

    fn queue_count(&self) -> usize {
        self.queues().len()
    }

    fn admin_irq_source(&self) -> Option<usize> {
        self.admin_irq_source_id()
    }

    fn submit_admin_command(&self, command: AdminCommand) -> Result<u16, InitError> {
        let command = self.queue_reinitialize_command(command)?;
        self.submit_lifecycle_admin(command)
    }

    fn take_admin_completion(&self) -> Option<AdminCompletion> {
        NvmeBlockOwner::take_admin_completion(self)
    }

    fn complete_admin_command(
        &self,
        command: AdminCommand,
        completion: AdminCompletion,
    ) -> Result<Option<AdminCommand>, InitError> {
        self.complete_reinitialize_admin(command, completion)
    }
}

impl DriverGeneric for NvmeBlockDriver {
    fn name(&self) -> &str {
        self.name
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl InitialController for NvmeBlockDriver {
    fn irq_sources(&self) -> IdList {
        self.inner
            .admin_irq_source_id()
            .map_or_else(IdList::none, |source_id| {
                let mut sources = IdList::none();
                sources.insert(source_id);
                sources
            })
    }

    fn take_irq_source(&mut self, source_id: usize) -> Option<BlockIrqSource> {
        if self.inner.admin_irq_source_id() != Some(source_id)
            || !self.inner.irq.take_initial_source(source_id)
        {
            return None;
        }
        Some(new_initial_irq_source(
            Arc::clone(&self.inner.irq),
            source_id,
        ))
    }

    fn poll_init(&mut self, input: InitInput) -> InitPoll<()> {
        self.initialization.poll(self.inner.as_ref(), input)
    }
}

impl Interface for NvmeBlockDriver {
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
        if self.initialization.is_ready() && self.namespace_if_ready().is_some() {
            ControllerInitEndpoint::Ready
        } else {
            ControllerInitEndpoint::Pending(self)
        }
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Interrupt(self)
    }

    fn device_info(&self) -> DeviceInfo {
        self.device_info_for()
    }

    fn queue_limits(&self) -> QueueLimits {
        self.limits_for()
    }

    fn create_queue(&mut self) -> Option<QueueHandle> {
        if !self.initialization.is_ready()
            || self.namespace_if_ready().is_none()
            || !self.inner.irq_supported
            || self.inner.admin_irq_source_id().is_none()
            || self.inner.irq.any_queue_source_taken()
        {
            return None;
        }
        let id = self.inner.next_queue_id.fetch_add(1, Ordering::Relaxed);
        if id >= u64::BITS as usize {
            return None;
        }
        let interrupt_sources = queue_interrupt_sources(
            self.inner.msix_interrupts,
            &self.inner.interrupt_vectors,
            id,
        );
        if interrupt_sources.is_empty() {
            return None;
        }

        let queue = self.inner.with_mut(|inner| {
            let queue = inner.nvme.take_io_queue(id)?;
            let depth = self.queue_depth.min(queue.depth().saturating_sub(1).max(1));
            let prp_lists = alloc_prp_lists(&inner.nvme, depth).ok()?;
            Some(NvmeQueueCore::new(
                id,
                depth,
                self.name,
                inner.namespace?,
                inner.nvme.dma_mask(),
                inner.nvme.page_size(),
                inner.nvme.max_transfer_bytes(),
                interrupt_sources,
                queue,
                prp_lists,
            ))
        })?;

        self.inner.register_queue(queue.clone());
        self.inner
            .created_queue_bits
            .fetch_or(1 << id, Ordering::Release);
        Some(QueueHandle::new(Box::new(NvmeBlockQueue::new(
            queue,
            Arc::clone(&self.inner),
        ))))
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        if !self.inner.irq_supported {
            return Err(BlkError::NotSupported);
        }
        if !self.initialization.is_ready() {
            if !self.inner.irq.initial_source_live() {
                return Err(BlkError::Other(
                    "NVMe initialization IRQ source is not live",
                ));
            }
            // The initialization FSM performs the first source unmask only
            // after retained admin CQ state has been reset.
            self.inner.irq.enable_delivery();
            return Ok(());
        }

        let required_sources = self.inner.required_io_irq_source_bits();
        if !self.inner.irq.all_queue_sources_live(required_sources) {
            return Err(BlkError::Other("NVMe I/O IRQ sources are not all live"));
        }

        // Publish endpoint readiness before unmasking either INTx or MSI-X. A
        // completion already resident in a CQ may assert immediately.
        self.inner.irq.arm_io_sources(required_sources);
        Ok(())
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        if !self.inner.irq_supported {
            return Err(BlkError::NotSupported);
        }
        self.inner.irq.disable_all();
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.irq_supported && self.inner.irq.delivery_enabled()
    }

    fn irq_sources(&self) -> IrqSourceList {
        let queue_bits = self.inner.created_queue_bits.load(Ordering::Acquire);
        if !self.inner.irq_supported || queue_bits == 0 {
            return Vec::new();
        }
        self.inner.irq_sources_from_queue_bits(queue_bits)
    }

    fn take_irq_source(&mut self, source_id: usize) -> Option<BlockIrqSource> {
        if !self.inner.irq_supported || self.inner.irq.initial_source_live() {
            return None;
        }
        let queue_bits = self.inner.source_queue_bits(
            source_id,
            self.inner.created_queue_bits.load(Ordering::Acquire),
        );
        if queue_bits == 0 {
            return None;
        }
        if !self.inner.irq.take_queue_source(source_id) {
            return None;
        }
        Some(new_queue_irq_source(
            Arc::clone(&self.inner.irq),
            source_id,
            IdList::from_bits(queue_bits),
        ))
    }
}

impl InterruptLifecycle for NvmeBlockDriver {
    fn controller_cookie(&self) -> usize {
        self.inner.controller_cookie()
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: rdif_block::ControllerEpoch,
        cause: RecoveryCause,
    ) -> Result<(), InitError> {
        self.lifecycle
            .begin_dma_quiesce(self.inner.as_ref(), epoch, cause)
    }

    fn poll_dma_quiesce(&mut self, input: InitInput) -> InitPoll<rdif_block::DmaQuiesced> {
        self.lifecycle.poll_dma_quiesce(self.inner.as_ref(), input)
    }

    fn enter_guest_owned(&mut self, quiesced: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        self.lifecycle
            .enter_guest_owned(self.inner.as_ref(), quiesced)
    }

    fn begin_reinitialize(&mut self, quiesced: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        self.lifecycle
            .begin_reinitialize(self.inner.as_ref(), quiesced)
    }

    fn poll_reinitialize(&mut self, input: InitInput) -> InitPoll<rdif_block::ControllerReady> {
        self.lifecycle.poll_reinitialize(self.inner.as_ref(), input)
    }
}
