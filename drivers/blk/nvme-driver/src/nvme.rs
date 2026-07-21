use alloc::{sync::Arc, vec::Vec};
use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::{CoherentArray, ContiguousArray, DeviceDma, DmaDirection, DmaOp};
use mmio_api::{Mmio, MmioAddr, MmioOp};
use rdif_block::InitError;

use crate::{
    command::{
        self, ControllerInfo, Feature, Identify, IdentifyActiveNamespaceList, IdentifyController,
        IdentifyNamespaceDataStructure,
    },
    err::*,
    initialization::InitialAdminCommand,
    lifecycle::{AdminCommand, AdminCompletion, queue_count_supported},
    queue::{CommandSet, NvmeCompletion, NvmeCompletionProbe, NvmeQueue},
    registers::NvmeReg,
};

pub struct Nvme {
    bar: NonNull<NvmeReg>,
    mmio_lease: NvmeMmioLease,
    dma: DeviceDma,
    admin_queue: Arc<NvmeQueue>,
    io_queues: Vec<Arc<NvmeQueue>>,
    io_queue_init: Vec<NvmeIoQueueInit>,
    num_ns: usize,
    sqes: u32,
    cqes: u32,
    page_size: usize,
    max_transfer_bytes: Option<usize>,
    io_queue_entries: usize,
    io_queue_pair_capacity: usize,
    io_queue_interrupts: bool,
    msix_interrupts: bool,
    interrupt_vectors: Vec<u16>,
    requested_io_queue_count: usize,
    identify_buffer: ContiguousArray<u8>,
    initial_namespace_id: Option<u32>,
    namespace: Option<Namespace>,
}

/// Independent capability for the NVMe INTMS/INTMC register pair.
///
/// The block owner retains the BAR mapping while this value is reachable.
/// Keeping this capability separate prevents hard-IRQ source masking from
/// borrowing the mutable controller/configuration object.
pub(crate) struct NvmeInterruptPort {
    bar: NonNull<NvmeReg>,
    _mmio_lease: NvmeMmioLease,
}

/// Shared proof that an NVMe BAR remains mapped for one MMIO capability.
///
/// Controller control, IRQ mask ports, and every queue doorbell clone this
/// lease so the v0.13 move-only parts can be torn down in any order without
/// leaving a live raw BAR pointer behind.
#[derive(Clone)]
pub(crate) struct NvmeMmioLease {
    _mapping: Option<Arc<Mmio>>,
}

/// Immutable queue geometry retained by controller initialization.
///
/// It is not a queue owner: SQ/CQ cursors and DMA allocations remain in the
/// move-only queue returned to the v0.13 I/O domain.
#[derive(Clone, Copy)]
struct NvmeIoQueueInit {
    qid: u32,
    sq_len: usize,
    cq_len: usize,
    sq_bus_addr: u64,
    cq_bus_addr: u64,
}

/// Hardware-invisible queue resources awaiting one activation commit.
pub(crate) struct PreparedNvmeIoQueues {
    queues: Vec<NvmeQueue>,
    init: Vec<NvmeIoQueueInit>,
}

const ADMIN_QUEUE_DEPTH: usize = 64;
const IO_SUBMISSION_QUEUE_DEPTH: usize = 64;
const IO_COMPLETION_QUEUE_DEPTH: usize = 16;
const NVM_SQ_ENTRY_SIZE: u32 = 6;
const NVM_CQ_ENTRY_SIZE: u32 = 4;
const ADMIN_COMMAND_ID: u16 = 0;
const MAX_IO_QUEUE_PAIRS: usize = u64::BITS as usize;

#[derive(Debug, Clone)]
pub struct Config {
    page_size: usize,
    max_io_queue_pairs: usize,
    io_queue_interrupts: bool,
    msix_interrupts: bool,
    interrupt_vectors: Vec<u16>,
}

impl Config {
    /// Defines a discovery resource ceiling, not the final runtime topology.
    ///
    /// The legacy interface realizes the full ceiling for compatibility. The
    /// v0.13 activator defers all I/O queue DMA allocation until its immutable
    /// [`rdif_block::ActivationPlan`] selects an exact count and depth.
    pub const fn new(page_size: usize, max_io_queue_pairs: usize) -> Self {
        Self {
            page_size,
            max_io_queue_pairs,
            io_queue_interrupts: false,
            msix_interrupts: false,
            interrupt_vectors: Vec::new(),
        }
    }

    pub fn with_intx_irq(mut self) -> Self {
        self.io_queue_interrupts = true;
        self.msix_interrupts = false;
        self.interrupt_vectors = Vec::from([0]);
        self
    }

    pub fn with_msix_vectors(mut self, vectors: impl Into<Vec<u16>>) -> Self {
        self.interrupt_vectors = vectors.into();
        self.io_queue_interrupts = !self.interrupt_vectors.is_empty();
        self.msix_interrupts = self.io_queue_interrupts;
        self
    }
}

impl Nvme {
    /// Maps and allocates an NVMe controller without issuing a hardware command.
    pub(crate) fn discover(
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
        config: Config,
    ) -> Result<Self> {
        validate_discovery_config(&config)?;
        mmio_api::init(mmio_op);
        let mmio = mmio_api::ioremap(bar_addr.into(), bar_size)?;
        let dma = DeviceDma::new_legacy(dma_mask, dma_op);
        Self::discover_mmio(mmio, dma, config, true)
    }

    /// Discovers only controller-wide resources for a two-phase activation.
    ///
    /// No I/O SQ/CQ DMA storage is allocated until the runtime supplies its
    /// exact activation plan.
    pub(crate) fn discover_for_activation(
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
        config: Config,
    ) -> Result<Self> {
        validate_discovery_config(&config)?;
        mmio_api::init(mmio_op);
        let mmio = mmio_api::ioremap(bar_addr.into(), bar_size)?;
        let dma = DeviceDma::new_legacy(dma_mask, dma_op);
        Self::discover_mmio(mmio, dma, config, false)
    }

    fn discover_mmio(
        mmio: Mmio,
        dma: DeviceDma,
        config: Config,
        allocate_legacy_io_queues: bool,
    ) -> Result<Self> {
        let mmio = Arc::new(mmio);
        let mmio_lease = NvmeMmioLease {
            _mapping: Some(Arc::clone(&mmio)),
        };
        let bar = NonNull::new(mmio.as_ptr())
            .expect("a successful MMIO mapping must have a non-null base")
            .cast::<NvmeReg>();
        let registers = unsafe {
            // SAFETY: `mmio` owns a mapping covering the controller BAR for
            // the lifetime of the returned discovery object.
            bar.as_ref()
        };
        validate_controller_capabilities(registers, config.page_size)?;
        let controller_queue_depth = registers.max_queue_entries();
        if controller_queue_depth < 2 {
            return Err(Error::Unknown(
                "NVMe controller queue capacity cannot retain one request",
            ));
        }
        let admin_queue = Arc::new(NvmeQueue::new(
            0,
            bar,
            &dma,
            config.page_size,
            ADMIN_QUEUE_DEPTH.min(controller_queue_depth),
            ADMIN_QUEUE_DEPTH.min(controller_queue_depth),
            mmio_lease.clone(),
        )?);
        let io_submission_entries = IO_SUBMISSION_QUEUE_DEPTH.min(controller_queue_depth);
        let io_completion_entries = IO_COMPLETION_QUEUE_DEPTH.min(controller_queue_depth);
        let io_queue_entries = io_submission_entries.min(io_completion_entries);
        let io_queues = if allocate_legacy_io_queues {
            allocate_owned_io_queues(
                bar,
                &dma,
                &mmio_lease,
                config.page_size,
                config.max_io_queue_pairs,
                io_queue_entries,
            )?
            .into_iter()
            .map(Arc::new)
            .collect()
        } else {
            Vec::new()
        };
        let io_queue_init = io_queues
            .iter()
            .map(|queue| NvmeIoQueueInit::from_queue(queue))
            .collect();
        let identify_buffer = dma.contiguous_array_zero_with_align::<u8>(
            config.page_size,
            config.page_size,
            DmaDirection::FromDevice,
        )?;

        let controller = Self {
            bar,
            mmio_lease,
            dma,
            admin_queue,
            io_queues,
            io_queue_init,
            num_ns: 0,
            sqes: NVM_SQ_ENTRY_SIZE,
            cqes: NVM_CQ_ENTRY_SIZE,
            page_size: config.page_size,
            max_transfer_bytes: None,
            io_queue_entries,
            io_queue_pair_capacity: config.max_io_queue_pairs,
            io_queue_interrupts: config.io_queue_interrupts,
            msix_interrupts: config.msix_interrupts,
            interrupt_vectors: config.interrupt_vectors,
            requested_io_queue_count: if allocate_legacy_io_queues {
                config.max_io_queue_pairs
            } else {
                0
            },
            identify_buffer,
            initial_namespace_id: None,
            namespace: None,
        };
        let interrupt_port = controller.interrupt_port();
        for vector in &controller.interrupt_vectors {
            interrupt_port.mask(u32::from(*vector));
        }
        Ok(controller)
    }

    pub fn dma_mask(&self) -> u64 {
        self.dma.dma_mask()
    }

    pub(crate) fn controller_identity(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.bar.as_ptr().expose_provenance())
            .expect("a mapped NVMe BAR has a nonzero identity")
    }

    pub(crate) fn controller_timeout_ns(&self) -> u64 {
        self.reg().timeout_ns()
    }

    pub(crate) fn begin_controller_disable(&self) {
        self.reg().begin_disable();
    }

    pub(crate) fn controller_ready(&self) -> bool {
        self.reg().is_ready()
    }

    pub(crate) fn controller_fatal(&self) -> bool {
        self.reg().is_fatal()
    }

    /// Programs the retained admin queue and starts controller enable.
    ///
    /// # Safety
    ///
    /// CC.RDY must be zero and the maintenance owner must have drained the
    /// registered IRQ action and every queue access.
    pub(crate) unsafe fn prepare_controller_reinitialize(&self) {
        unsafe { self.admin_queue.reset_after_controller_disable() };
        self.nvme_configure_admin_queue();
        self.reg()
            .begin_enable(self.sqes, self.cqes, self.page_size);
    }

    pub(crate) fn admin_queue(&self) -> Arc<NvmeQueue> {
        Arc::clone(&self.admin_queue)
    }

    /// Creates the read-only hard-IRQ CQ phase capability without sharing the
    /// mutable admin queue owner.
    pub(crate) fn admin_completion_probe(&self) -> NvmeCompletionProbe {
        self.admin_queue.completion_probe()
    }

    /// Submits one command from the controller maintenance owner.
    pub(crate) fn submit_admin_command(&mut self, command: CommandSet) {
        self.admin_queue.submit_admin_command(command);
    }

    /// Consumes one admin completion from the controller maintenance owner.
    pub(crate) fn take_admin_completion(&mut self) -> Option<NvmeCompletion> {
        self.admin_queue.take_owner_completion()
    }

    /// Programs the first admin queue and enables a disabled controller.
    ///
    /// # Safety
    ///
    /// The controller must have acknowledged `CC.RDY=0`, and no initialization
    /// admin command may be pending. Hard IRQ has no admin-CQ capability.
    pub(crate) unsafe fn prepare_initial_enable(&self) {
        unsafe { self.admin_queue.reset_after_controller_disable() };
        self.nvme_configure_admin_queue();
        self.reg()
            .begin_enable(NVM_SQ_ENTRY_SIZE, NVM_CQ_ENTRY_SIZE, self.page_size);
    }

    pub(crate) fn build_initial_admin_command(
        &self,
        command: InitialAdminCommand,
    ) -> core::result::Result<CommandSet, InitError> {
        match command {
            InitialAdminCommand::IdentifyController => {
                Ok(self.identify_command(IdentifyController::new()))
            }
            InitialAdminCommand::SetQueueCount => {
                let requested = u32::try_from(self.requested_io_queue_count)
                    .ok()
                    .and_then(|count| count.checked_sub(1))
                    .ok_or(InitError::Hardware("invalid NVMe I/O queue count"))?;
                Ok(CommandSet::set_features_with_cid(
                    Feature::NumberOfQueues {
                        nsq: requested,
                        ncq: requested,
                    },
                    ADMIN_COMMAND_ID,
                ))
            }
            InitialAdminCommand::CreateCompletionQueue { queue_index } => {
                let queue = self.initial_io_queue(queue_index)?;
                let vector = self
                    .interrupt_vector_for_queue(queue_index)
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
            InitialAdminCommand::CreateSubmissionQueue { queue_index } => {
                let queue = self.initial_io_queue(queue_index)?;
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
            InitialAdminCommand::IdentifyNamespaceList => {
                Ok(self.identify_command(IdentifyActiveNamespaceList::new()))
            }
            InitialAdminCommand::IdentifyNamespace { namespace_id } => {
                Ok(self.identify_command(IdentifyNamespaceDataStructure::new(namespace_id)))
            }
        }
    }

    pub(crate) fn build_reidentify_admin_command(
        &self,
        command: AdminCommand,
    ) -> core::result::Result<CommandSet, InitError> {
        match command {
            AdminCommand::IdentifyController => {
                Ok(self.identify_command(IdentifyController::new()))
            }
            AdminCommand::IdentifyNamespaceList => {
                Ok(self.identify_command(IdentifyActiveNamespaceList::new()))
            }
            AdminCommand::IdentifyNamespace { namespace_id } => {
                Ok(self.identify_command(IdentifyNamespaceDataStructure::new(namespace_id)))
            }
            AdminCommand::SetQueueCount { .. }
            | AdminCommand::CreateCompletionQueue { .. }
            | AdminCommand::CreateSubmissionQueue { .. } => Err(InitError::InvalidState),
        }
    }

    pub(crate) fn validate_reidentified_controller(&self) -> core::result::Result<(), InitError> {
        let controller = self.parse_identify(IdentifyController::new());
        validate_controller_entry_sizes(&controller)?;
        if controller.number_of_namespaces == 0 {
            return Err(InitError::Hardware(
                "NVMe controller lost every namespace during recovery",
            ));
        }
        if controller_max_transfer_bytes(self.page_size, controller.mdts) != self.max_transfer_bytes
        {
            return Err(InitError::Hardware(
                "NVMe controller transfer geometry changed during recovery",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_reidentified_namespace_list(
        &self,
    ) -> core::result::Result<u32, InitError> {
        let retained = self.namespace.ok_or(InitError::InvalidState)?;
        let namespaces = self.parse_identify(IdentifyActiveNamespaceList::new());
        namespaces
            .contains(&retained.id)
            .then_some(retained.id)
            .ok_or(InitError::Hardware(
                "NVMe active namespace changed during recovery",
            ))
    }

    /// Validates the v0.13 logical-device catalog, including an intentionally
    /// empty namespace set discovered during the initial activation.
    pub(crate) fn validate_reidentified_namespace_list_discovering(
        &self,
    ) -> core::result::Result<Option<u32>, InitError> {
        let namespaces = self.parse_identify(IdentifyActiveNamespaceList::new());
        match self.namespace {
            Some(retained) => namespaces
                .contains(&retained.id)
                .then_some(Some(retained.id))
                .ok_or(InitError::Hardware(
                    "NVMe active namespace changed during recovery",
                )),
            None if namespaces.iter().all(|namespace| *namespace == 0) => Ok(None),
            None => Err(InitError::Hardware(
                "NVMe namespace appeared without a new logical-device publication",
            )),
        }
    }

    pub(crate) fn validate_reidentified_namespace(
        &self,
        namespace_id: u32,
    ) -> core::result::Result<(), InitError> {
        let retained = self.namespace.ok_or(InitError::InvalidState)?;
        if retained.id != namespace_id {
            return Err(InitError::InvalidState);
        }
        let identified = self
            .parse_identify(IdentifyNamespaceDataStructure::new(namespace_id))
            .ok_or(InitError::Hardware(
                "NVMe namespace identify data is empty after recovery",
            ))?;
        validate_reidentified_namespace(retained, &identified)
    }

    pub(crate) fn complete_initial_admin(
        &mut self,
        command: InitialAdminCommand,
        completion: AdminCompletion,
    ) -> core::result::Result<Option<InitialAdminCommand>, InitError> {
        self.complete_initial_admin_with_policy(command, completion, false)
    }

    /// Completes one discovery command while allowing an empty namespace set.
    ///
    /// The v0.13 activation boundary can publish a ready controller with no
    /// logical devices. The legacy single-device interface retains its
    /// historical fail-closed behavior through [`Self::complete_initial_admin`].
    pub(crate) fn complete_initial_admin_discovering(
        &mut self,
        command: InitialAdminCommand,
        completion: AdminCompletion,
    ) -> core::result::Result<Option<InitialAdminCommand>, InitError> {
        self.complete_initial_admin_with_policy(command, completion, true)
    }

    fn complete_initial_admin_with_policy(
        &mut self,
        command: InitialAdminCommand,
        completion: AdminCompletion,
        allow_empty_namespaces: bool,
    ) -> core::result::Result<Option<InitialAdminCommand>, InitError> {
        match command {
            InitialAdminCommand::IdentifyController => {
                let controller = self.parse_identify(IdentifyController::new());
                validate_controller_entry_sizes(&controller)?;
                self.num_ns = controller.number_of_namespaces as usize;
                if self.num_ns == 0 && !allow_empty_namespaces {
                    return Err(InitError::Hardware("NVMe controller has no namespace"));
                }
                self.sqes = NVM_SQ_ENTRY_SIZE;
                self.cqes = NVM_CQ_ENTRY_SIZE;
                self.max_transfer_bytes =
                    controller_max_transfer_bytes(self.page_size, controller.mdts);
                Ok(Some(InitialAdminCommand::SetQueueCount))
            }
            InitialAdminCommand::SetQueueCount => {
                if !queue_count_supported(completion.result, self.requested_io_queue_count) {
                    return Err(InitError::Hardware(
                        "NVMe controller did not allocate the requested queue count",
                    ));
                }
                Ok(Some(InitialAdminCommand::CreateCompletionQueue {
                    queue_index: 0,
                }))
            }
            InitialAdminCommand::CreateCompletionQueue { queue_index } => {
                Ok(Some(InitialAdminCommand::CreateSubmissionQueue {
                    queue_index,
                }))
            }
            InitialAdminCommand::CreateSubmissionQueue { queue_index } => {
                let next = queue_index.saturating_add(1);
                if next < self.requested_io_queue_count {
                    Ok(Some(InitialAdminCommand::CreateCompletionQueue {
                        queue_index: next,
                    }))
                } else {
                    Ok(Some(InitialAdminCommand::IdentifyNamespaceList))
                }
            }
            InitialAdminCommand::IdentifyNamespaceList => {
                let namespaces = self.parse_identify(IdentifyActiveNamespaceList::new());
                let Some(namespace_id) = namespaces.first().copied() else {
                    if allow_empty_namespaces {
                        self.initial_namespace_id = None;
                        self.namespace = None;
                        return Ok(None);
                    }
                    return Err(InitError::Hardware(
                        "NVMe controller has no active namespace",
                    ));
                };
                self.initial_namespace_id = Some(namespace_id);
                Ok(Some(InitialAdminCommand::IdentifyNamespace {
                    namespace_id,
                }))
            }
            InitialAdminCommand::IdentifyNamespace { namespace_id } => {
                if self.initial_namespace_id != Some(namespace_id) {
                    return Err(InitError::InvalidState);
                }
                let namespace = self
                    .parse_identify(IdentifyNamespaceDataStructure::new(namespace_id))
                    .ok_or(InitError::Hardware("NVMe namespace identify data is empty"))?;
                if namespace.metadata_size != 0 {
                    return Err(InitError::Hardware(
                        "NVMe namespaces with metadata are not supported",
                    ));
                }
                self.namespace = Some(Namespace {
                    id: namespace_id,
                    lba_size: namespace.lba_size as usize,
                    lba_count: namespace.namespace_size,
                    metadata_size: namespace.metadata_size as usize,
                });
                Ok(None)
            }
        }
    }

    pub(crate) const fn namespace_if_ready(&self) -> Option<Namespace> {
        self.namespace
    }

    fn identify_command<T: Identify>(&self, mut identify: T) -> CommandSet {
        self.identify_buffer.prepare_for_device_all();
        let command = identify.command_set_mut();
        command.cdw0 =
            CommandSet::cdw0_from_opcode_with_cid(command::Opcode::IDENTIFY, ADMIN_COMMAND_ID);
        command.cdw10 = T::CNS;
        command.prp1 = self.identify_buffer.dma_addr().as_u64();
        *command
    }

    fn parse_identify<T: Identify>(&self, identify: T) -> T::Output {
        self.identify_buffer
            .read_from_device(self.identify_buffer.len(), |data| identify.parse(data))
    }

    fn initial_io_queue(&self, index: usize) -> core::result::Result<&NvmeIoQueueInit, InitError> {
        self.io_queue_init
            .get(index)
            .ok_or(InitError::Hardware("missing preallocated NVMe I/O queue"))
    }

    fn interrupt_vector_for_queue(&self, queue_index: usize) -> Option<u16> {
        if self.msix_interrupts {
            self.interrupt_vectors.get(queue_index).copied()
        } else {
            Some(0)
        }
    }

    // config admin queue
    // 1. set admin queue(cq && sq) size
    // 2. set admin queue(cq && sq) dma address
    // 3. enable ctrl
    fn nvme_configure_admin_queue(&self) {
        self.reg().set_admin_submission_and_completion_queue_size(
            self.admin_queue.sq_len(),
            self.admin_queue.cq_len(),
        );

        self.reg()
            .set_admin_submission_queue_base_address(self.admin_queue.sq_bus_addr());

        self.reg()
            .set_admin_completion_queue_base_address(self.admin_queue.cq_bus_addr());
    }

    pub(crate) fn page_size(&self) -> usize {
        self.page_size
    }

    pub(crate) const fn max_transfer_bytes(&self) -> Option<usize> {
        self.max_transfer_bytes
    }

    /// Returns the common SQ/CQ entry capacity frozen during discovery.
    pub(crate) const fn io_queue_entries(&self) -> usize {
        self.io_queue_entries
    }

    /// Discovery-time resource ceiling used by the v0.13 plan validator.
    pub(crate) const fn io_queue_pair_capacity(&self) -> usize {
        self.io_queue_pair_capacity
    }

    /// Allocates exactly the queue count and descriptor depth selected by the
    /// immutable activation plan.
    pub(crate) fn prepare_selected_io_queues(
        &self,
        count: usize,
        depth: usize,
    ) -> Result<PreparedNvmeIoQueues> {
        if !self.io_queues.is_empty()
            || !self.io_queue_init.is_empty()
            || self.requested_io_queue_count != 0
        {
            return Err(Error::Unknown(
                "NVMe I/O queues were already allocated before activation",
            ));
        }
        if count == 0 || count > self.io_queue_pair_capacity {
            return Err(Error::Unknown(
                "selected NVMe I/O queue count exceeds discovery resources",
            ));
        }
        let queue_entries = depth
            .checked_add(1)
            .filter(|entries| *entries <= self.io_queue_entries)
            .ok_or(Error::Unknown(
                "selected NVMe queue depth exceeds controller resources",
            ))?;
        let queues = allocate_owned_io_queues(
            self.bar,
            &self.dma,
            &self.mmio_lease,
            self.page_size,
            count,
            queue_entries,
        )?;
        let init = queues.iter().map(NvmeIoQueueInit::from_queue).collect();
        Ok(PreparedNvmeIoQueues { queues, init })
    }

    pub(crate) fn io_queue_interrupts_enabled(&self) -> bool {
        self.io_queue_interrupts
    }

    pub(crate) fn msix_interrupts_enabled(&self) -> bool {
        self.io_queue_interrupts && self.msix_interrupts
    }

    pub(crate) fn interrupt_vectors(&self) -> &[u16] {
        &self.interrupt_vectors
    }

    /// Creates an independent INTMS/INTMC capability.
    ///
    /// The capability retains its own mapping lease so IRQ registration and
    /// source-control teardown cannot outlive the BAR they access.
    pub(crate) fn interrupt_port(&self) -> NvmeInterruptPort {
        NvmeInterruptPort {
            bar: self.bar,
            _mmio_lease: self.mmio_lease.clone(),
        }
    }

    /// Retains one preallocated queue for a fixed maintenance ownership domain.
    ///
    /// The controller control part keeps its own reference only to program and
    /// reset the queue under an explicit DMA-quiesced lifecycle transition.
    /// Normal SQ/CQ progress belongs exclusively to the I/O-domain reference.
    pub(crate) fn io_queue(&self, index: usize) -> Option<Arc<NvmeQueue>> {
        self.io_queues.get(index).cloned()
    }

    pub(crate) fn alloc_prp_list(&self) -> Result<CoherentArray<u64>> {
        self.dma
            .coherent_array_zero_with_align(
                self.page_size / core::mem::size_of::<u64>(),
                self.page_size,
            )
            .map_err(Into::into)
    }

    fn reg(&self) -> &NvmeReg {
        unsafe { self.bar.as_ref() }
    }
}

impl NvmeInterruptPort {
    pub(crate) fn mask(&self, vector: u32) {
        self.reg().mask_interrupt_vector(vector);
    }

    pub(crate) fn unmask(&self, vector: u32) {
        self.reg().unmask_interrupt_vector(vector);
    }

    fn reg(&self) -> &NvmeReg {
        unsafe {
            // SAFETY: this capability retains the MMIO mapping lease, and
            // discovery validated that it covers INTMS and INTMC.
            self.bar.as_ref()
        }
    }

    #[cfg(test)]
    /// Creates a register capability over test-owned backing storage.
    ///
    /// # Safety
    ///
    /// `bar` must remain valid and suitably aligned for [`NvmeReg`] for the
    /// complete returned capability lifetime.
    pub(crate) unsafe fn from_test_bar(bar: NonNull<u8>) -> Self {
        Self {
            bar: bar.cast(),
            _mmio_lease: NvmeMmioLease { _mapping: None },
        }
    }
}

// SAFETY: the capability accesses only the atomic device-side INTMS/INTMC
// write-one register pair. The capability retains the mapping and lifecycle
// activation serializes owner-thread rearm with hard-IRQ masking.
unsafe impl Send for NvmeInterruptPort {}

// SAFETY: concurrent writes to INTMS/INTMC are independent write-one bit
// operations defined by the NVMe register interface; no Rust memory is aliased.
unsafe impl Sync for NvmeInterruptPort {}

unsafe impl Send for Nvme {}

impl NvmeIoQueueInit {
    fn from_queue(queue: &NvmeQueue) -> Self {
        Self {
            qid: queue.qid,
            sq_len: queue.sq_len(),
            cq_len: queue.cq_len(),
            sq_bus_addr: queue.sq_bus_addr(),
            cq_bus_addr: queue.cq_bus_addr(),
        }
    }
}

impl PreparedNvmeIoQueues {
    pub(crate) fn queues(&self) -> &[NvmeQueue] {
        &self.queues
    }

    /// Commits immutable creation geometry and returns the unique queue owners.
    pub(crate) fn commit(self, controller: &mut Nvme) -> Vec<NvmeQueue> {
        debug_assert!(controller.io_queues.is_empty());
        debug_assert!(controller.io_queue_init.is_empty());
        debug_assert_eq!(controller.requested_io_queue_count, 0);
        controller.requested_io_queue_count = self.queues.len();
        controller.io_queue_init = self.init;
        self.queues
    }
}

fn allocate_owned_io_queues(
    bar: NonNull<NvmeReg>,
    dma: &DeviceDma,
    mmio_lease: &NvmeMmioLease,
    page_size: usize,
    count: usize,
    queue_entries: usize,
) -> Result<Vec<NvmeQueue>> {
    let mut queues = Vec::with_capacity(count);
    for queue_index in 0..count {
        let queue_id = u32::try_from(queue_index + 1)
            .map_err(|_| Error::Unknown("NVMe I/O queue ID exceeds u32"))?;
        queues.push(NvmeQueue::new(
            queue_id,
            bar,
            dma,
            page_size,
            queue_entries,
            queue_entries,
            mmio_lease.clone(),
        )?);
    }
    Ok(queues)
}

fn validate_discovery_config(config: &Config) -> Result<()> {
    if config.page_size < 4096 || !config.page_size.is_power_of_two() {
        return Err(Error::Unknown(
            "NVMe controller page size must be a power of two of at least 4096 bytes",
        ));
    }
    if config.max_io_queue_pairs == 0 || config.max_io_queue_pairs > MAX_IO_QUEUE_PAIRS {
        return Err(Error::Unknown("invalid NVMe I/O queue count"));
    }
    if !config.io_queue_interrupts || config.interrupt_vectors.is_empty() {
        return Err(Error::Unknown(
            "NVMe IRQ-only runtime requires an interrupt source",
        ));
    }
    if !config.msix_interrupts
        && config
            .interrupt_vectors
            .iter()
            .any(|vector| u32::from(*vector) >= u32::BITS)
    {
        return Err(Error::Unknown(
            "NVMe interrupt vector cannot be masked by INTMS",
        ));
    }
    if config.msix_interrupts
        && (config.interrupt_vectors.len() < config.max_io_queue_pairs
            || config.interrupt_vectors.first() != Some(&0))
    {
        return Err(Error::Unknown(
            "NVMe MSI-X runtime requires queue zero on vector zero and one mapping per queue",
        ));
    }
    Ok(())
}

fn validate_controller_capabilities(registers: &NvmeReg, page_size: usize) -> Result<()> {
    if !registers.supports_nvm_command_set() {
        return Err(Error::Unknown(
            "NVMe controller does not support the NVM command set",
        ));
    }
    if !registers.supports_page_size(page_size) {
        return Err(Error::Unknown(
            "NVMe controller does not support the requested memory page size",
        ));
    }
    Ok(())
}

fn validate_controller_entry_sizes(
    controller: &ControllerInfo,
) -> core::result::Result<(), InitError> {
    let sq_supported = controller.sqes_min <= NVM_SQ_ENTRY_SIZE as u8
        && NVM_SQ_ENTRY_SIZE as u8 <= controller.sqes_max;
    let cq_supported = controller.cqes_min <= NVM_CQ_ENTRY_SIZE as u8
        && NVM_CQ_ENTRY_SIZE as u8 <= controller.cqes_max;
    if !sq_supported || !cq_supported {
        return Err(InitError::Hardware(
            "NVMe controller does not support mandatory NVM queue entry sizes",
        ));
    }
    Ok(())
}

fn controller_max_transfer_bytes(page_size: usize, mdts: u8) -> Option<usize> {
    if mdts == 0 {
        None
    } else {
        Some(page_size.checked_shl(u32::from(mdts)).unwrap_or(usize::MAX))
    }
}

fn validate_reidentified_namespace(
    retained: Namespace,
    identified: &crate::command::NamespaceDataStructure,
) -> core::result::Result<(), InitError> {
    let identified_lba_size = usize::try_from(identified.lba_size).map_err(|_| {
        InitError::Hardware("NVMe namespace LBA size is not representable after recovery")
    })?;
    let identified_metadata_size = usize::try_from(identified.metadata_size).map_err(|_| {
        InitError::Hardware("NVMe namespace metadata size is not representable after recovery")
    })?;
    if retained.lba_count != identified.namespace_size
        || retained.lba_size != identified_lba_size
        || retained.metadata_size != identified_metadata_size
    {
        return Err(InitError::Hardware(
            "NVMe namespace geometry changed during recovery",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Namespace {
    pub id: u32,
    pub lba_size: usize,
    pub lba_count: u64,
    pub metadata_size: usize,
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::{
        Config, Namespace, controller_max_transfer_bytes, validate_discovery_config,
        validate_reidentified_namespace,
    };
    use crate::command::NamespaceDataStructure;

    #[test]
    fn config_requires_an_explicit_irq_topology_and_can_enable_intx() {
        let config = Config::new(4096, 1);
        assert!(!config.io_queue_interrupts);
        assert!(!config.msix_interrupts);
        assert!(config.interrupt_vectors.is_empty());
        assert!(validate_discovery_config(&config).is_err());

        let irq_config = config.with_intx_irq();
        assert!(irq_config.io_queue_interrupts);
        assert!(!irq_config.msix_interrupts);
        assert_eq!(irq_config.interrupt_vectors, [0]);
        assert!(validate_discovery_config(&irq_config).is_ok());
    }

    #[test]
    fn config_can_enable_msix_per_queue_vectors() {
        let config = Config::new(4096, 2).with_msix_vectors([0, 1]);

        assert!(config.io_queue_interrupts);
        assert!(config.msix_interrupts);
        assert_eq!(config.interrupt_vectors, [0, 1]);
        assert!(validate_discovery_config(&config).is_ok());
    }

    #[test]
    fn config_requires_the_first_published_queue_to_route_admin_vector_zero() {
        let config = Config::new(4096, 2).with_msix_vectors([1, 0]);

        assert!(
            validate_discovery_config(&config).is_err(),
            "recovery needs a permanent vector-zero handler even when only queue zero is created"
        );
    }

    #[test]
    fn config_accepts_msix_vectors_outside_controller_intms() {
        let config = Config::new(4096, 2).with_msix_vectors([0, u32::BITS as u16]);

        assert!(
            validate_discovery_config(&config).is_ok(),
            "MSI-X vectors are masked in the PCI MSI-X table, not NVMe INTMS"
        );
    }

    #[test]
    fn config_rejects_queues_that_cannot_be_named_by_rdif_irq_events() {
        let queue_count = u64::BITS as usize + 1;
        let config = Config::new(4096, queue_count).with_msix_vectors(vec![0; queue_count]);

        assert!(
            validate_discovery_config(&config).is_err(),
            "every initialized queue must fit the fixed RDIF queue-event mask"
        );
    }

    #[test]
    fn controller_mdts_zero_means_unrestricted_transfer_size() {
        assert_eq!(controller_max_transfer_bytes(4096, 0), None);
    }

    #[test]
    fn controller_mdts_scales_with_controller_page_size() {
        assert_eq!(controller_max_transfer_bytes(4096, 7), Some(512 * 1024));
    }

    #[test]
    fn recovery_rejects_changed_namespace_geometry() {
        let retained = Namespace {
            id: 7,
            lba_size: 512,
            lba_count: 4096,
            metadata_size: 0,
        };
        let matching = NamespaceDataStructure {
            namespace_size: 4096,
            namespace_capacity: 4096,
            namespace_used: 1024,
            lba_size: 512,
            metadata_size: 0,
        };
        assert!(validate_reidentified_namespace(retained, &matching).is_ok());

        let changed = NamespaceDataStructure {
            namespace_size: 8192,
            ..matching
        };
        assert!(validate_reidentified_namespace(retained, &changed).is_err());
    }
}
