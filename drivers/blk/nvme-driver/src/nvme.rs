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
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub page_size: usize,
    pub io_queue_pair_count: usize,
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
        let dma = DeviceDma::new(dma_mask, dma_op);
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
            self.admin_queue.sq.len(),
            self.admin_queue.cq.len(),
        );

        self.reg()
            .set_admin_submission_queue_base_address(self.admin_queue.sq.bus_addr());

        self.reg()
            .set_admin_completion_queue_base_address(self.admin_queue.cq.bus_addr());
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
                io_queue.cq.len() as _,
                io_queue.cq.bus_addr(),
                true,
                true,
                0,
            );
            self.admin_queue.command_sync(data)?;

            let data = CommandSet::create_io_submission_queue(
                io_queue.qid,
                io_queue.sq.len() as _,
                io_queue.sq.bus_addr(),
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

#[derive(Debug, Clone, Copy)]
pub struct Namespace {
    pub id: u32,
    pub lba_size: usize,
    pub lba_count: usize,
    pub metadata_size: usize,
}
