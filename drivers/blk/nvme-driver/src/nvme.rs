use alloc::vec::Vec;
use core::ptr::NonNull;

use dma_api::{CoherentArray, DeviceDma, DmaDirection, DmaOp};
use log::{debug, info};
use mmio_api::{Mmio, MmioAddr, MmioOp};

use crate::{
    command::{
        self, ControllerInfo, Feature, Identify, IdentifyActiveNamespaceList, IdentifyController,
        IdentifyNamespaceDataStructure,
    },
    err::*,
    queue::{CommandSet, NvmeQueue},
    registers::NvmeReg,
};

pub struct Nvme {
    bar: NonNull<NvmeReg>,
    _mmio: Option<Mmio>,
    dma: DeviceDma,
    admin_queue: NvmeQueue,
    io_queues: Vec<Option<NvmeQueue>>,
    num_ns: usize,
    sqes: u32,
    cqes: u32,
    page_size: usize,
    max_transfer_bytes: Option<usize>,
    io_queue_interrupts: bool,
    msix_interrupts: bool,
    interrupt_vectors: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub page_size: usize,
    pub io_queue_pair_count: usize,
    pub io_queue_interrupts: bool,
    pub interrupt_vector: u32,
    pub msix_interrupts: bool,
    pub interrupt_vectors: Vec<u16>,
}

impl Config {
    pub const fn new(page_size: usize, io_queue_pair_count: usize) -> Self {
        Self {
            page_size,
            io_queue_pair_count,
            io_queue_interrupts: false,
            interrupt_vector: 0,
            msix_interrupts: false,
            interrupt_vectors: Vec::new(),
        }
    }

    pub fn with_intx_irq(mut self) -> Self {
        self.io_queue_interrupts = true;
        self.interrupt_vector = 0;
        self.msix_interrupts = false;
        self.interrupt_vectors = Vec::from([0]);
        self
    }

    pub fn with_msix_vectors(mut self, vectors: impl Into<Vec<u16>>) -> Self {
        self.interrupt_vectors = vectors.into();
        self.io_queue_interrupts = !self.interrupt_vectors.is_empty();
        self.msix_interrupts = self.io_queue_interrupts;
        self.interrupt_vector = self
            .interrupt_vectors
            .first()
            .copied()
            .map(u32::from)
            .unwrap_or(0);
        self
    }

    fn interrupt_vector_for_queue(&self, queue_index: usize) -> u32 {
        self.interrupt_vectors
            .get(queue_index)
            .copied()
            .map(u32::from)
            .unwrap_or(self.interrupt_vector)
    }
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
        let admin_queue = NvmeQueue::new(0, bar, &dma, config.page_size, 64, 64)?;

        assert!(config.io_queue_pair_count > 0);

        let mut s = Self {
            bar,
            _mmio: mmio,
            dma,
            admin_queue,
            io_queues: Vec::new(),
            num_ns: 0,
            sqes: 6,
            cqes: 4,
            page_size: config.page_size,
            max_transfer_bytes: None,
            io_queue_interrupts: config.io_queue_interrupts,
            msix_interrupts: config.msix_interrupts,
            interrupt_vectors: config.interrupt_vectors.clone(),
        };

        let version = s.version();

        info!(
            "NVME @{bar:?} init begin, version: {}.{}.{} ",
            version.0, version.1, version.2
        );

        s.init(config)?;

        Ok(s)
    }

    pub fn dma_mask(&self) -> u64 {
        self.dma.dma_mask()
    }

    fn reset(&mut self) {
        self.reg().reset();
    }

    fn reset_and_setup_controller_info(&mut self) -> Result<ControllerInfo> {
        self.reset();
        self.nvme_configure_admin_queue();
        self.reg().ready_for_read_controller_info();

        self.get_identfy(IdentifyController::new())
    }

    fn init(&mut self, config: Config) -> Result {
        let controller = self.reset_and_setup_controller_info()?;

        debug!("Controller: {:?}", controller);

        self.sqes = controller.sqes_min as _;
        self.cqes = controller.cqes_min as _;
        self.reset();
        self.nvme_configure_admin_queue();
        self.reg().setup_cc(self.sqes, self.cqes);
        let controller = self.get_identfy(IdentifyController::new())?;

        debug!("Controller: {:?}", controller);

        self.num_ns = controller.number_of_namespaces as _;
        self.max_transfer_bytes = controller_max_transfer_bytes(config.page_size, controller.mdts);
        if config.io_queue_interrupts {
            for vector in &config.interrupt_vectors {
                self.mask_interrupt_vector(u32::from(*vector));
            }
        }
        self.config_io_queue(config)?;

        debug!("IO queue ok.");
        loop {
            let ns = self.get_identfy(IdentifyNamespaceDataStructure::new(1))?;
            if let Some(ns) = ns {
                debug!("Namespace: {:?}", ns);
                break;
            }
        }
        debug!("Namespace ok.");
        Ok(())
    }

    pub fn namespace_list(&mut self) -> Result<Vec<Namespace>> {
        let id_list = self.get_identfy(IdentifyActiveNamespaceList::new())?;
        let mut out = Vec::new();

        for id in id_list {
            let ns = self
                .get_identfy(IdentifyNamespaceDataStructure::new(id))?
                .unwrap();

            out.push(Namespace {
                id,
                lba_size: ns.lba_size as _,
                lba_count: ns.namespace_size as _,
                metadata_size: ns.metadata_size as _,
            });
        }

        Ok(out)
    }

    // config admin queue
    // 1. set admin queue(cq && sq) size
    // 2. set admin queue(cq && sq) dma address
    // 3. enable ctrl
    fn nvme_configure_admin_queue(&mut self) {
        self.reg().set_admin_submission_and_completion_queue_size(
            self.admin_queue.sq_len(),
            self.admin_queue.cq_len(),
        );

        self.reg()
            .set_admin_submission_queue_base_address(self.admin_queue.sq_bus_addr());

        self.reg()
            .set_admin_completion_queue_base_address(self.admin_queue.cq_bus_addr());
    }

    fn config_io_queue(&mut self, config: Config) -> Result {
        let num = config.io_queue_pair_count;
        // 设置 io queue 数量
        let cmd = CommandSet::set_features(Feature::NumberOfQueues {
            nsq: num as u32 - 1,
            ncq: num as u32 - 1,
        });
        self.admin_queue.command_sync(cmd)?;

        for i in 0..num {
            let id = (i + 1) as u32;
            let io_queue = NvmeQueue::new(
                id,
                self.bar,
                &self.dma,
                config.page_size,
                2usize.pow(self.sqes as _),
                2usize.pow(self.cqes as _),
            )?;

            let data = CommandSet::create_io_completion_queue(
                io_queue.qid,
                io_queue.cq_len() as _,
                io_queue.cq_bus_addr(),
                true,
                config.io_queue_interrupts,
                config.interrupt_vector_for_queue(i),
            );
            self.admin_queue.command_sync(data)?;

            let data = CommandSet::create_io_submission_queue(
                io_queue.qid,
                io_queue.sq_len() as _,
                io_queue.sq_bus_addr(),
                true,
                0,
                io_queue.qid,
                0,
            );

            self.admin_queue.command_sync(data)?;

            self.io_queues.push(Some(io_queue));
        }

        Ok(())
    }

    pub fn io_queue_count(&self) -> usize {
        self.io_queues.len()
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub(crate) const fn max_transfer_bytes(&self) -> Option<usize> {
        self.max_transfer_bytes
    }

    pub fn io_queue_interrupts_enabled(&self) -> bool {
        self.io_queue_interrupts
    }

    pub fn interrupt_vector(&self) -> u32 {
        self.interrupt_vectors
            .first()
            .copied()
            .map(u32::from)
            .unwrap_or(0)
    }

    pub fn msix_interrupts_enabled(&self) -> bool {
        self.io_queue_interrupts && self.msix_interrupts
    }

    pub fn interrupt_vectors(&self) -> &[u16] {
        &self.interrupt_vectors
    }

    pub fn mask_interrupt_vector(&mut self, vector: u32) {
        self.reg().mask_interrupt_vector(vector);
    }

    pub fn unmask_interrupt_vector(&mut self, vector: u32) {
        self.reg().unmask_interrupt_vector(vector);
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

    pub fn get_identfy<T: Identify>(&mut self, mut want: T) -> Result<T::Output> {
        let cmd = want.command_set_mut();

        cmd.cdw0 = CommandSet::cdw0_from_opcode(command::Opcode::IDENTIFY);
        cmd.cdw10 = T::CNS;

        let buff = self.dma.contiguous_array_zero_with_align::<u8>(
            0x1000,
            0x1000,
            DmaDirection::FromDevice,
        )?;
        cmd.prp1 = buff.dma_addr().as_u64();

        self.admin_queue.command_sync(*cmd)?;

        let data = buff.read_from_device(buff.len(), |data| data.to_vec());
        let res = want.parse(&data);
        Ok(res)
    }

    pub fn block_write_sync(
        &mut self,
        ns: &Namespace,
        block_start: u64,
        buff: &[u8],
    ) -> Result<()> {
        assert!(
            buff.len().is_multiple_of(ns.lba_size),
            "buffer size must be multiple of lba size"
        );

        let mut dma_buff = self.dma.contiguous_array_zero_with_align::<u8>(
            buff.len(),
            ns.lba_size,
            DmaDirection::ToDevice,
        )?;
        dma_buff.copy_to_device_from_slice(buff);

        let blk_num = dma_buff.len() / ns.lba_size;

        let cmd = CommandSet::nvm_cmd_write(
            ns.id,
            dma_buff.dma_addr().as_u64(),
            block_start,
            blk_num as _,
        );

        self.io_queues
            .get_mut(0)
            .and_then(Option::as_mut)
            .ok_or(Error::Unknown("missing IO queue"))?
            .command_sync(cmd)?;

        Ok(())
    }

    pub fn block_read_sync(
        &mut self,
        ns: &Namespace,
        block_start: u64,
        buff: &mut [u8],
    ) -> Result<()> {
        assert!(
            buff.len().is_multiple_of(ns.lba_size),
            "buffer size must be multiple of lba size"
        );

        let dma_buff = self.dma.contiguous_array_zero_with_align::<u8>(
            buff.len(),
            ns.lba_size,
            DmaDirection::FromDevice,
        )?;

        let blk_num = dma_buff.len() / ns.lba_size;

        let cmd = CommandSet::nvm_cmd_read(
            ns.id,
            dma_buff.dma_addr().as_u64(),
            block_start,
            blk_num as _,
        );

        self.io_queues
            .get_mut(0)
            .and_then(Option::as_mut)
            .ok_or(Error::Unknown("missing IO queue"))?
            .command_sync(cmd)?;
        dma_buff.copy_from_device_to_slice(buff);
        Ok(())
    }

    pub fn version(&self) -> (usize, usize, usize) {
        self.reg().version()
    }

    fn reg(&self) -> &NvmeReg {
        unsafe { self.bar.as_ref() }
    }
}

unsafe impl Send for Nvme {}

fn controller_max_transfer_bytes(page_size: usize, mdts: u8) -> Option<usize> {
    if mdts == 0 {
        None
    } else {
        Some(page_size.checked_shl(u32::from(mdts)).unwrap_or(usize::MAX))
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
    fn config_defaults_to_polling_and_can_enable_intx() {
        let config = Config::new(4096, 1);
        assert!(!config.io_queue_interrupts);
        assert_eq!(config.interrupt_vector, 0);
        assert!(!config.msix_interrupts);
        assert!(config.interrupt_vectors.is_empty());

        let irq_config = config.with_intx_irq();
        assert!(irq_config.io_queue_interrupts);
        assert_eq!(irq_config.interrupt_vector, 0);
        assert!(!irq_config.msix_interrupts);
        assert_eq!(irq_config.interrupt_vectors, [0]);
    }

    #[test]
    fn config_can_enable_msix_per_queue_vectors() {
        let config = Config::new(4096, 2).with_msix_vectors([4, 5]);

        assert!(config.io_queue_interrupts);
        assert!(config.msix_interrupts);
        assert_eq!(config.interrupt_vector, 4);
        assert_eq!(config.interrupt_vector_for_queue(0), 4);
        assert_eq!(config.interrupt_vector_for_queue(1), 5);
    }

    #[test]
    fn controller_mdts_zero_means_unrestricted_transfer_size() {
        assert_eq!(controller_max_transfer_bytes(4096, 0), None);
    }

    #[test]
    fn controller_mdts_scales_with_controller_page_size() {
        assert_eq!(controller_max_transfer_bytes(4096, 7), Some(512 * 1024));
    }
}
