#[cfg(any(
    feature = "fs-ng",
    all(feature = "irq", any(feature = "fs", feature = "fs-ng"))
))]
use alloc::boxed::Box;
#[cfg(feature = "fs-ng")]
use alloc::vec::Vec;
#[cfg(all(feature = "irq", any(feature = "fs", feature = "fs-ng")))]
use core::{
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

#[cfg(all(feature = "irq", any(feature = "fs", feature = "fs-ng")))]
use ax_driver::block::BlockIrqHandler;

#[cfg(feature = "fs-ng")]
mod root;
#[cfg(any(feature = "fs-ng", test))]
pub(crate) mod volume;

#[cfg(all(feature = "irq", any(feature = "fs", feature = "fs-ng")))]
const BLOCK_IRQ_SLOTS: usize = 16;

#[cfg(feature = "fs-ng")]
struct FsNgBlockDevice(ax_driver::block::Block);

#[cfg(feature = "fs-ng")]
impl ax_fs_ng::FsBlockDevice for FsNgBlockDevice {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn num_blocks(&self) -> u64 {
        self.0.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.0.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_errno::AxResult {
        self.0.read_block(block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_errno::AxResult {
        self.0.write_block(block_id, buf)
    }

    fn flush(&mut self) -> ax_errno::AxResult {
        self.0.flush()
    }
}

#[cfg(all(feature = "irq", any(feature = "fs", feature = "fs-ng")))]
static BLOCK_IRQ_HANDLERS: [AtomicPtr<BlockIrqHandler>; BLOCK_IRQ_SLOTS] =
    [const { AtomicPtr::new(ptr::null_mut()) }; BLOCK_IRQ_SLOTS];

#[cfg(any(feature = "fs", feature = "fs-ng"))]
pub(crate) fn register_irq_handlers(blocks: &mut [ax_driver::block::Block]) {
    #[cfg(feature = "irq")]
    {
        for block in blocks {
            let Some((irq_num, handler)) = block.take_irq_handler() else {
                continue;
            };
            register_irq_handler(irq_num, handler);
        }
    }

    #[cfg(not(feature = "irq"))]
    {
        let _ = blocks;
    }
}

#[cfg(all(feature = "irq", any(feature = "fs", feature = "fs-ng")))]
fn register_irq_handler(irq_num: usize, handler: BlockIrqHandler) {
    let handler = Box::into_raw(Box::new(handler));
    for (slot, source) in BLOCK_IRQ_HANDLERS.iter().enumerate() {
        if source
            .compare_exchange(
                ptr::null_mut(),
                handler,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            continue;
        }

        if axklib::irq::register(irq_num, BLOCK_IRQ_THUNKS[slot]) {
            // The runtime IRQ entry is now installed and the slot points at the
            // handler, so enabling device-side IRQs cannot race a null slot.
            unsafe { &*handler }.enable_irq();
            return;
        }

        source.store(ptr::null_mut(), Ordering::Release);
        unsafe {
            drop(Box::from_raw(handler));
        }
        warn!("failed to register block irq handler for irq {irq_num}");
        return;
    }

    unsafe {
        drop(Box::from_raw(handler));
    }
    warn!("no free block irq handler slot for irq {irq_num}");
}

#[cfg(all(feature = "irq", any(feature = "fs", feature = "fs-ng")))]
fn handle_block_irq(slot: usize) {
    let handler = BLOCK_IRQ_HANDLERS[slot].load(Ordering::Acquire);
    let Some(handler) = (unsafe { handler.as_ref() }) else {
        return;
    };
    let _ = handler.handle();
}

#[cfg(all(feature = "irq", any(feature = "fs", feature = "fs-ng")))]
fn handle_block_irq_slot<const SLOT: usize>(_: usize) {
    handle_block_irq(SLOT);
}

#[cfg(all(feature = "irq", any(feature = "fs", feature = "fs-ng")))]
const BLOCK_IRQ_THUNKS: [fn(usize); BLOCK_IRQ_SLOTS] = [
    handle_block_irq_slot::<0>,
    handle_block_irq_slot::<1>,
    handle_block_irq_slot::<2>,
    handle_block_irq_slot::<3>,
    handle_block_irq_slot::<4>,
    handle_block_irq_slot::<5>,
    handle_block_irq_slot::<6>,
    handle_block_irq_slot::<7>,
    handle_block_irq_slot::<8>,
    handle_block_irq_slot::<9>,
    handle_block_irq_slot::<10>,
    handle_block_irq_slot::<11>,
    handle_block_irq_slot::<12>,
    handle_block_irq_slot::<13>,
    handle_block_irq_slot::<14>,
    handle_block_irq_slot::<15>,
];

#[cfg(all(feature = "fs-ng", feature = "plat-dyn"))]
pub(crate) fn init_dyn_fs_ng(bootargs: Option<&str>) {
    #[cfg(target_os = "none")]
    init_fs_ng_from_blocks(take_block_devices(), bootargs);

    #[cfg(not(target_os = "none"))]
    let _ = bootargs;
}

#[cfg(all(feature = "fs-ng", not(feature = "plat-dyn")))]
pub(crate) fn init_static_fs_ng() {
    init_fs_ng_from_blocks(take_block_devices(), None);
}

#[cfg(feature = "fs-ng")]
fn take_block_devices() -> Vec<ax_driver::block::Block> {
    let mut devices = ax_driver::block::take_block_devices();
    register_irq_handlers(&mut devices);
    devices
}

#[cfg(feature = "fs-ng")]
fn init_fs_ng_from_blocks(blocks: Vec<ax_driver::block::Block>, bootargs: Option<&str>) {
    let block_devs = blocks
        .into_iter()
        .map(|dev| Box::new(FsNgBlockDevice(dev)) as Box<dyn ax_fs_ng::FsBlockDevice>);
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
