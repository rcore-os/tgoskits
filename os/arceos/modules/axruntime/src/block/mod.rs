#[cfg(all(feature = "fs-ng", any(not(feature = "plat-dyn"), target_os = "none")))]
use alloc::vec::Vec;

#[cfg(all(feature = "fs-ng", any(not(feature = "plat-dyn"), target_os = "none")))]
mod root;
#[cfg(any(
    all(feature = "fs-ng", any(not(feature = "plat-dyn"), target_os = "none")),
    test
))]
pub(crate) mod volume;

#[cfg(all(feature = "fs-ng", any(not(feature = "plat-dyn"), target_os = "none")))]
struct FsNgBlockDevice(ax_driver::block::Block);

#[cfg(all(feature = "fs-ng", any(not(feature = "plat-dyn"), target_os = "none")))]
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

#[cfg(any(
    all(
        feature = "fs",
        not(feature = "fs-ng"),
        any(not(feature = "plat-dyn"), target_os = "none")
    ),
    all(feature = "fs-ng", any(not(feature = "plat-dyn"), target_os = "none"))
))]
pub(crate) fn register_irq_handlers(blocks: &mut [ax_driver::block::Block]) {
    // Block queues are driven through the rdif-block polled path for now. Avoid
    // installing block handlers on shared legacy IRQ lines used by net devices.
    let _ = blocks;
}

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

#[cfg(all(feature = "fs-ng", any(not(feature = "plat-dyn"), target_os = "none")))]
fn take_block_devices() -> Vec<ax_driver::block::Block> {
    let mut devices = ax_driver::block::take_block_devices();
    register_irq_handlers(&mut devices);
    devices
}

#[cfg(all(feature = "fs-ng", any(not(feature = "plat-dyn"), target_os = "none")))]
fn init_fs_ng_from_blocks(blocks: Vec<ax_driver::block::Block>, bootargs: Option<&str>) {
    let block_devs = blocks.into_iter().map(|dev| {
        alloc::boxed::Box::new(FsNgBlockDevice(dev))
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
