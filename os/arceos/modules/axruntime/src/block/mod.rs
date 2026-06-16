#[cfg(feature = "fs-ng")]
use alloc::vec::Vec;

#[cfg(any(all(feature = "fs", not(feature = "fs-ng")), feature = "fs-ng"))]
mod irq_support {
    #[cfg(feature = "irq")]
    pub(crate) struct BlockIrqState {
        handler: ax_driver::block::BlockIrqHandler,
    }

    #[cfg(feature = "irq")]
    pub(crate) type BlockIrqRegistration = crate::irq::HandlerRegistration<BlockIrqState>;

    #[cfg(not(feature = "irq"))]
    pub(crate) type BlockIrqRegistration = ();

    #[cfg(feature = "irq")]
    unsafe fn handle_block_irq(
        _ctx: ax_hal::irq::IrqContext,
        data: core::ptr::NonNull<()>,
    ) -> ax_hal::irq::IrqReturn {
        let state = unsafe { data.cast::<BlockIrqState>().as_ref() };
        let _event = state.handler.handle();
        ax_hal::irq::IrqReturn::Handled
    }

    pub(crate) fn register_irq_handler(
        block: &mut ax_driver::block::Block,
    ) -> Option<BlockIrqRegistration> {
        #[cfg(feature = "irq")]
        {
            let name = alloc::string::String::from(block.name());
            let (irq, handler) = block.take_irq_handler()?;
            register_shared_for_block(
                block,
                name,
                irq,
                BlockIrqState { handler },
                handle_block_irq,
                ax_driver::block::Block::enable_irq,
                ax_driver::block::Block::disable_irq,
            )
        }

        #[cfg(not(feature = "irq"))]
        {
            let _ = block;
            None
        }
    }

    #[cfg(feature = "irq")]
    fn register_shared_for_block(
        block: &mut ax_driver::block::Block,
        name: alloc::string::String,
        irq: usize,
        state: BlockIrqState,
        handler: ax_hal::irq::RawIrqHandler,
        enable_irq: impl FnOnce(&mut ax_driver::block::Block),
        disable_irq: impl FnOnce(&mut ax_driver::block::Block),
    ) -> Option<BlockIrqRegistration> {
        match crate::irq::HandlerRegistration::register_shared(name, irq, state, handler) {
            Ok(registration) => {
                enable_irq(block);
                Some(registration)
            }
            Err(_) => {
                disable_irq(block);
                None
            }
        }
    }
}

#[cfg(any(all(feature = "fs", not(feature = "fs-ng")), feature = "fs-ng"))]
pub(crate) use irq_support::{BlockIrqRegistration, register_irq_handler};

#[cfg(feature = "fs-ng")]
mod root;
#[cfg(any(feature = "fs-ng", test))]
pub(crate) mod volume;

#[cfg(feature = "fs-ng")]
struct FsNgBlockDevice {
    _irq: Option<BlockIrqRegistration>,
    block: ax_driver::block::Block,
}

#[cfg(feature = "fs-ng")]
impl FsNgBlockDevice {
    fn new(mut block: ax_driver::block::Block) -> Self {
        let irq = register_irq_handler(&mut block);
        Self { _irq: irq, block }
    }
}

#[cfg(feature = "fs-ng")]
impl ax_fs_ng::FsBlockDevice for FsNgBlockDevice {
    fn name(&self) -> &str {
        self.block.name()
    }

    fn num_blocks(&self) -> u64 {
        self.block.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.block.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_errno::AxResult {
        self.block.read_block(block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_errno::AxResult {
        self.block.write_block(block_id, buf)
    }

    fn flush(&mut self) -> ax_errno::AxResult {
        self.block.flush()
    }
}

#[cfg(all(feature = "fs-ng", feature = "plat-dyn"))]
pub(crate) fn init_dyn_fs_ng(bootargs: Option<&str>) {
    init_fs_ng_from_blocks(take_block_devices(), bootargs);
}

#[cfg(all(feature = "fs-ng", not(feature = "plat-dyn")))]
pub(crate) fn init_static_fs_ng() {
    init_fs_ng_from_blocks(take_block_devices(), None);
}

#[cfg(feature = "fs-ng")]
fn take_block_devices() -> Vec<ax_driver::block::Block> {
    ax_driver::block::take_block_devices()
}

#[cfg(feature = "fs-ng")]
fn init_fs_ng_from_blocks(blocks: Vec<ax_driver::block::Block>, bootargs: Option<&str>) {
    let block_devs = blocks.into_iter().map(|dev| {
        alloc::boxed::Box::new(FsNgBlockDevice::new(dev))
            as alloc::boxed::Box<dyn ax_fs_ng::FsBlockDevice>
    });
    let root_spec = root::parse_root_spec(bootargs);
    let mut disks = root::collect_disks(block_devs);
    let candidates = root::collect_root_candidates(&disks);
    let (selected_disk_index, selected_partition) =
        root::select_root_candidate(&candidates, &root_spec).unwrap_or_else(|| {
            panic!("failed to determine root device from available block devices")
        });
    let selected_disk_pos = disks
        .iter()
        .position(|disk| disk.disk_index == selected_disk_index)
        .unwrap_or_else(|| panic!("selected root disk disappeared during initialization"));
    let selected = disks.swap_remove(selected_disk_pos);
    let selected_partition_info = selected_partition.and_then(|part_index| {
        selected
            .partitions
            .iter()
            .find(|partition| partition.info.index == part_index)
    });
    let description = root::describe_selection(selected.disk_index, selected_partition_info);
    let region = selected_partition_info.map_or_else(
        || ax_fs_ng::BlockRegion::from_num_blocks(selected.dev.num_blocks()),
        |part| part.info.region,
    );

    ax_fs_ng::init_filesystem(selected.dev, region, &description);
}
