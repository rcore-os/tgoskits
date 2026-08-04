use alloc::{sync::Arc, vec::Vec};
use core::{
    mem,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use dma_api::{CoherentArray, ContiguousArray, DeviceDma, DmaDirection, DmaOp};
use log::{debug, info};
use mmio_api::{Mmio, MmioAddr, MmioOp};

use crate::{
    command::{
        self, Feature, Identify, IdentifyActiveNamespaceList, IdentifyController,
        IdentifyNamespaceDataStructure,
    },
    err::*,
    queue::{CommandSet, NvmeQueue},
    registers::NvmeReg,
};

const ADMIN_QUEUE_DEPTH: usize = 64;
const DEFAULT_IO_QUEUE_DEPTH: usize = 64;
const IDENTIFY_BYTES: usize = 4096;

pub struct Nvme {
    bar: NonNull<NvmeReg>,
    _mmio: Option<Mmio>,
    dma: DeviceDma,
    admin_queue: NvmeQueue,
    io_queues: Vec<Option<NvmeQueue>>,
    config: Config,
    init_state: NvmeInitState,
    namespace: Option<Namespace>,
    negotiated_io_queues: usize,
    num_ns: usize,
    sqes: u32,
    cqes: u32,
    page_size: usize,
    max_transfer_bytes: Option<usize>,
    intx_io_ready: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub struct Config {
    page_size: usize,
    io_queue_pair_count: usize,
    admin_vector: u16,
    io_vectors: Vec<u16>,
    msix: bool,
}

impl Config {
    /// Creates the explicit legacy INTx single-queue mode.
    pub fn intx(page_size: usize) -> Self {
        Self {
            page_size,
            io_queue_pair_count: 1,
            admin_vector: 0,
            io_vectors: Vec::from([0]),
            msix: false,
        }
    }

    /// Creates MSI-X mode. The first vector is reserved for the admin queue
    /// and every remaining vector maps to one I/O queue.
    pub fn msix(page_size: usize, vectors: impl Into<Vec<u16>>) -> Result<Self> {
        let vectors = vectors.into();
        if vectors.len() < 2 {
            return Err(Error::Unknown(
                "NVMe MSI-X requires one admin and at least one I/O vector",
            ));
        }
        Ok(Self {
            page_size,
            io_queue_pair_count: (vectors.len() - 1).min(64),
            admin_vector: vectors[0],
            io_vectors: vectors[1..].iter().copied().take(64).collect(),
            msix: true,
        })
    }

    fn io_vector_for_queue(&self, queue_index: usize) -> Option<u16> {
        self.io_vectors.get(queue_index).copied()
    }
}

pub(crate) enum NvmeInitProgress {
    RegisterPending,
    WaitingForIrq,
    Ready(Namespace),
}

enum NvmeInitState {
    NotStarted,
    Disabling,
    Enabling,
    IdentifyController(IdentifyPending<IdentifyController>),
    SetQueueCount,
    CreateCompletionQueue {
        index: usize,
        queue: NvmeQueue,
    },
    CreateSubmissionQueue {
        index: usize,
        queue: NvmeQueue,
    },
    IdentifyNamespaceList(IdentifyPending<IdentifyActiveNamespaceList>),
    IdentifyNamespace {
        namespace_id: u32,
        pending: IdentifyPending<IdentifyNamespaceDataStructure>,
    },
    Ready,
    Failed,
}

struct IdentifyPending<T> {
    parser: T,
    buffer: ContiguousArray<u8>,
}

impl Nvme {
    pub fn new(
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
        config: Config,
    ) -> Result<Self> {
        mmio_api::init(mmio_op);
        let mmio = mmio_api::ioremap(bar_addr.into(), bar_size)?;
        let dma = DeviceDma::new_legacy(dma_mask, dma_op);
        Self::new_mmio(mmio, dma, config)
    }

    fn new_mmio(mmio: Mmio, dma: DeviceDma, config: Config) -> Result<Self> {
        let bar = NonNull::new(mmio.as_ptr()).expect("mmio mapping must not be null");
        Self::new_with_bar(bar.cast(), Some(mmio), dma, config)
    }

    fn new_with_bar(
        bar: NonNull<NvmeReg>,
        mmio: Option<Mmio>,
        dma: DeviceDma,
        config: Config,
    ) -> Result<Self> {
        if config.page_size < 4096 || !config.page_size.is_power_of_two() {
            return Err(Error::Unknown("invalid NVMe controller page size"));
        }
        let register = unsafe { bar.as_ref() };
        let controller_depth = register.max_queue_entries();
        if controller_depth < 2 {
            return Err(Error::Unknown(
                "NVMe controller queue depth cannot hold a command",
            ));
        }
        let admin_depth = controller_depth.min(ADMIN_QUEUE_DEPTH);
        let admin_queue = NvmeQueue::new(0, bar, &dma, config.page_size, admin_depth, admin_depth)?;
        let version = register.version();
        info!(
            "NVME @{bar:?} deferred init, version: {}.{}.{}",
            version.0, version.1, version.2
        );

        let intx_io_ready = Arc::new(AtomicBool::new(false));
        let nvme = Self {
            bar,
            _mmio: mmio,
            dma,
            admin_queue,
            io_queues: Vec::new(),
            page_size: config.page_size,
            config,
            init_state: NvmeInitState::NotStarted,
            namespace: None,
            negotiated_io_queues: 0,
            num_ns: 0,
            sqes: 6,
            cqes: 4,
            max_transfer_bytes: None,
            intx_io_ready,
        };
        nvme.mask_all_interrupt_sources();
        Ok(nvme)
    }

    pub fn dma_mask(&self) -> u64 {
        self.dma.dma_mask()
    }

    pub(crate) fn start_initialization(&mut self) -> Result<NvmeInitProgress> {
        if !matches!(self.init_state, NvmeInitState::NotStarted) {
            return Err(Error::Unknown("NVMe initialization already started"));
        }
        self.reg().begin_disable();
        self.init_state = NvmeInitState::Disabling;
        Ok(NvmeInitProgress::RegisterPending)
    }

    pub(crate) fn retry_initialization(&mut self) -> Result<NvmeInitProgress> {
        let state = mem::replace(&mut self.init_state, NvmeInitState::Failed);
        match state {
            NvmeInitState::Disabling => {
                if !self.reg().is_disabled() {
                    self.init_state = NvmeInitState::Disabling;
                    return Ok(NvmeInitProgress::RegisterPending);
                }
                self.configure_admin_queue();
                self.reg()
                    .begin_enable(self.sqes, self.cqes, self.page_size)?;
                self.init_state = NvmeInitState::Enabling;
                Ok(NvmeInitProgress::RegisterPending)
            }
            NvmeInitState::Enabling => {
                if self.reg().has_fatal_status() {
                    return Err(Error::Unknown("NVMe controller reported fatal status"));
                }
                if !self.reg().is_ready() {
                    self.init_state = NvmeInitState::Enabling;
                    return Ok(NvmeInitProgress::RegisterPending);
                }
                let pending = self.submit_identify(IdentifyController::new())?;
                self.init_state = NvmeInitState::IdentifyController(pending);
                Ok(NvmeInitProgress::WaitingForIrq)
            }
            other => {
                self.init_state = other;
                Err(Error::Unknown(
                    "NVMe register retry outside a register transition",
                ))
            }
        }
    }

    pub(crate) fn handle_admin_irq(&mut self) -> Result<NvmeInitProgress> {
        let Some(completion) = self.admin_queue.take_completion_after_irq() else {
            return self.current_init_progress();
        };
        // Exactly one admin command is outstanding during initialization.
        // Publish the consumed CQ head before staging the next command so the
        // controller can reuse the entry and generate its next interrupt.
        self.admin_queue.commit_completion_head();
        if !completion.status.is_success() {
            self.init_state = NvmeInitState::Failed;
            debug!(
                "NVMe admin command failed: status={:#x}, result={:#x}",
                completion.status.0, completion.result
            );
            return Err(Error::Unknown("NVMe admin command failed"));
        }

        let state = mem::replace(&mut self.init_state, NvmeInitState::Failed);
        match state {
            NvmeInitState::IdentifyController(pending) => {
                let controller = parse_identify(pending);
                self.sqes = u32::from(controller.sqes_min);
                self.cqes = u32::from(controller.cqes_min);
                self.num_ns = controller.number_of_namespaces as usize;
                self.max_transfer_bytes =
                    controller_max_transfer_bytes(self.reg().minimum_page_size(), controller.mdts);
                let requested = self.config.io_queue_pair_count;
                let command = CommandSet::set_features(Feature::NumberOfQueues {
                    nsq: requested as u32 - 1,
                    ncq: requested as u32 - 1,
                });
                self.admin_queue.submit_admin_data(command);
                self.init_state = NvmeInitState::SetQueueCount;
                Ok(NvmeInitProgress::WaitingForIrq)
            }
            NvmeInitState::SetQueueCount => {
                let result = completion.result as u32;
                let submission_queues = usize::from((result & 0xffff) as u16) + 1;
                let completion_queues = usize::from((result >> 16) as u16) + 1;
                self.negotiated_io_queues = self
                    .config
                    .io_queue_pair_count
                    .min(submission_queues)
                    .min(completion_queues)
                    .min(self.config.io_vectors.len())
                    .min(64);
                if self.negotiated_io_queues == 0 {
                    return Err(Error::Unknown(
                        "NVMe controller exposed no IRQ-backed I/O queue",
                    ));
                }
                self.submit_create_completion_queue(0)?;
                Ok(NvmeInitProgress::WaitingForIrq)
            }
            NvmeInitState::CreateCompletionQueue { index, queue } => {
                let command = CommandSet::create_io_submission_queue(
                    queue.qid,
                    queue.sq_len() as u32,
                    queue.sq_bus_addr(),
                    true,
                    0,
                    queue.qid,
                    0,
                );
                self.admin_queue.submit_admin_data(command);
                self.init_state = NvmeInitState::CreateSubmissionQueue { index, queue };
                Ok(NvmeInitProgress::WaitingForIrq)
            }
            NvmeInitState::CreateSubmissionQueue { index, queue } => {
                self.io_queues.push(Some(queue));
                if index + 1 < self.negotiated_io_queues {
                    self.submit_create_completion_queue(index + 1)?;
                } else {
                    let pending = self.submit_identify(IdentifyActiveNamespaceList::new())?;
                    self.init_state = NvmeInitState::IdentifyNamespaceList(pending);
                }
                Ok(NvmeInitProgress::WaitingForIrq)
            }
            NvmeInitState::IdentifyNamespaceList(pending) => {
                let namespace_ids = parse_identify(pending);
                let namespace_id = *namespace_ids
                    .first()
                    .ok_or(Error::Unknown("NVMe has no active namespace"))?;
                let pending =
                    self.submit_identify(IdentifyNamespaceDataStructure::new(namespace_id))?;
                self.init_state = NvmeInitState::IdentifyNamespace {
                    namespace_id,
                    pending,
                };
                Ok(NvmeInitProgress::WaitingForIrq)
            }
            NvmeInitState::IdentifyNamespace {
                namespace_id,
                pending,
            } => {
                let namespace = parse_identify(pending)
                    .ok_or(Error::Unknown("active NVMe namespace disappeared"))?;
                let namespace = Namespace {
                    id: namespace_id,
                    lba_size: namespace.lba_size as usize,
                    lba_count: namespace.namespace_size as usize,
                    metadata_size: namespace.metadata_size as usize,
                };
                self.namespace = Some(namespace);
                self.init_state = NvmeInitState::Ready;
                self.intx_io_ready.store(true, Ordering::Release);
                Ok(NvmeInitProgress::Ready(namespace))
            }
            NvmeInitState::Ready => {
                self.init_state = NvmeInitState::Ready;
                Ok(NvmeInitProgress::Ready(
                    self.namespace
                        .ok_or(Error::Unknown("NVMe namespace is unavailable"))?,
                ))
            }
            other => {
                self.init_state = other;
                Err(Error::Unknown("unexpected NVMe admin completion"))
            }
        }
    }

    fn current_init_progress(&self) -> Result<NvmeInitProgress> {
        match &self.init_state {
            NvmeInitState::Disabling | NvmeInitState::Enabling => {
                Ok(NvmeInitProgress::RegisterPending)
            }
            NvmeInitState::Ready => Ok(NvmeInitProgress::Ready(
                self.namespace
                    .ok_or(Error::Unknown("NVMe namespace is unavailable"))?,
            )),
            NvmeInitState::Failed => Err(Error::Unknown("NVMe initialization failed")),
            _ => Ok(NvmeInitProgress::WaitingForIrq),
        }
    }

    fn configure_admin_queue(&mut self) {
        self.reg().set_admin_submission_and_completion_queue_size(
            self.admin_queue.sq_len(),
            self.admin_queue.cq_len(),
        );
        self.reg()
            .set_admin_submission_queue_base_address(self.admin_queue.sq_bus_addr());
        self.reg()
            .set_admin_completion_queue_base_address(self.admin_queue.cq_bus_addr());
    }

    fn submit_identify<T: Identify>(&mut self, mut parser: T) -> Result<IdentifyPending<T>> {
        let command = parser.command_set_mut();
        command.cdw0 = CommandSet::cdw0_from_opcode(command::Opcode::IDENTIFY);
        command.cdw10 = T::CNS;
        let buffer = self.dma.contiguous_array_zero_with_align::<u8>(
            IDENTIFY_BYTES,
            self.page_size,
            DmaDirection::FromDevice,
        )?;
        command.prp1 = buffer.dma_addr().as_u64();
        self.admin_queue.submit_admin_data(*command);
        Ok(IdentifyPending { parser, buffer })
    }

    fn submit_create_completion_queue(&mut self, index: usize) -> Result<()> {
        let id = u32::try_from(index + 1).map_err(|_| Error::Unknown("NVMe queue id overflow"))?;
        let depth = self.reg().max_queue_entries().min(DEFAULT_IO_QUEUE_DEPTH);
        let queue = NvmeQueue::new(id, self.bar, &self.dma, self.page_size, depth, depth)?;
        let vector = self
            .config
            .io_vector_for_queue(index)
            .ok_or(Error::Unknown("missing NVMe I/O interrupt vector"))?;
        let command = CommandSet::create_io_completion_queue(
            queue.qid,
            queue.cq_len() as u32,
            queue.cq_bus_addr(),
            true,
            true,
            u32::from(vector),
        );
        self.admin_queue.submit_admin_data(command);
        self.init_state = NvmeInitState::CreateCompletionQueue { index, queue };
        Ok(())
    }

    pub fn io_queue_count(&self) -> usize {
        self.io_queues.len()
    }

    pub fn configured_io_queue_count(&self) -> usize {
        self.config.io_queue_pair_count
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub(crate) const fn max_transfer_bytes(&self) -> Option<usize> {
        self.max_transfer_bytes
    }

    pub fn msix_interrupts_enabled(&self) -> bool {
        self.config.msix
    }

    pub(crate) fn admin_interrupt_source(&self) -> usize {
        usize::from(self.config.admin_vector)
    }

    pub(crate) fn available_io_interrupt_sources(&self) -> usize {
        self.config.io_vectors.len()
    }

    pub(crate) fn interrupt_source_for_io_queue(&self, queue_index: usize) -> Option<usize> {
        self.config
            .io_vector_for_queue(queue_index)
            .map(usize::from)
    }

    pub(crate) fn intx_io_ready(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.intx_io_ready)
    }

    pub(crate) fn register_ptr(&self) -> NonNull<NvmeReg> {
        self.bar
    }

    pub(crate) fn unmask_interrupt_source(&mut self, source_id: usize) -> Result<()> {
        let valid = source_id == self.admin_interrupt_source()
            || self
                .config
                .io_vectors
                .contains(&u16::try_from(source_id).map_err(|_| {
                    Error::Unknown("interrupt source is outside the NVMe vector range")
                })?);
        if !valid {
            return Err(Error::Unknown(
                "interrupt source does not belong to this NVMe controller",
            ));
        }
        if !self.config.msix {
            self.reg().unmask_interrupt_vector(0);
        }
        Ok(())
    }

    pub(crate) fn mask_all_interrupt_sources(&self) {
        if !self.config.msix {
            self.reg().mask_interrupt_vector(0);
        }
    }

    pub(crate) fn shutdown(&mut self) {
        self.mask_all_interrupt_sources();
        self.reg().begin_disable();
        self.intx_io_ready.store(false, Ordering::Release);
    }

    pub(crate) fn shutdown_complete(&self) -> bool {
        self.reg().is_disabled()
    }

    pub(crate) fn take_io_queue(&mut self, index: usize) -> Option<NvmeQueue> {
        self.io_queues.get_mut(index)?.take()
    }

    pub(crate) fn alloc_prp_list(&self) -> Result<CoherentArray<u64>> {
        self.dma
            .coherent_array_zero_with_align(
                self.page_size / core::mem::size_of::<u64>(),
                self.page_size,
            )
            .map_err(Into::into)
    }

    pub fn version(&self) -> (usize, usize, usize) {
        self.reg().version()
    }

    fn reg(&self) -> &NvmeReg {
        unsafe { self.bar.as_ref() }
    }
}

unsafe impl Send for Nvme {}

fn parse_identify<T: Identify>(pending: IdentifyPending<T>) -> T::Output {
    pending
        .buffer
        .read_from_device(pending.buffer.len(), |data| pending.parser.parse(data))
}

fn controller_max_transfer_bytes(minimum_page_size: usize, mdts: u8) -> Option<usize> {
    if mdts == 0 {
        None
    } else {
        Some(
            minimum_page_size
                .checked_shl(u32::from(mdts))
                .unwrap_or(usize::MAX),
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Namespace {
    pub id: u32,
    pub lba_size: usize,
    pub lba_count: usize,
    pub metadata_size: usize,
}

#[cfg(test)]
mod tests {
    use super::{Config, controller_max_transfer_bytes};

    #[test]
    fn config_has_no_polling_mode() {
        let intx = Config::intx(4096);
        assert_eq!(intx.io_queue_pair_count, 1);
        assert!(!intx.msix);
        assert_eq!(intx.admin_vector, 0);
        assert_eq!(intx.io_vectors, [0]);
    }

    #[test]
    fn msix_reserves_the_first_vector_for_admin() {
        let config = Config::msix(4096, [4, 5, 6]).unwrap();
        assert!(config.msix);
        assert_eq!(config.admin_vector, 4);
        assert_eq!(config.io_queue_pair_count, 2);
        assert_eq!(config.io_vector_for_queue(0), Some(5));
        assert_eq!(config.io_vector_for_queue(1), Some(6));
        assert!(Config::msix(4096, [4]).is_err());
    }

    #[test]
    fn controller_mdts_zero_means_unrestricted_transfer_size() {
        assert_eq!(controller_max_transfer_bytes(4096, 0), None);
    }

    #[test]
    fn controller_mdts_scales_with_cap_mpsmin() {
        assert_eq!(controller_max_transfer_bytes(4096, 7), Some(512 * 1024));
        assert_eq!(
            controller_max_transfer_bytes(64 * 1024, 1),
            Some(128 * 1024)
        );
    }
}
