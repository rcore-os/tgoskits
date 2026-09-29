//! Pure eligibility policy for the destructive block axtest scratch target.
//!
//! The destructive write tests need a region that no mounted filesystem or
//! partition-table metadata uses. Whether a device qualifies is the safety
//! boundary that keeps raw writes away from mounted data, so the decision is
//! kept free of device I/O here and covered by deterministic host unit
//! tests; the device scan and marker read live in
//! [`crate::block_runtime_axtest`].

use ax_fs_ng::BlockRegion;
use rdif_block::DeviceInfo;

/// First-sector marker identifying a dedicated scratch disk.
pub(crate) const SCRATCH_DISK_MARKER: &[u8] = b"TGOS_BLOCK_SCRATCH_V1\n";

pub(crate) const WRITE_TEST_BLOCKS: u32 = 8;
pub(crate) const MIXED_TEST_BLOCKS: u32 = 8;
pub(crate) const MULTI_DESCRIPTOR_TEST_BLOCKS: u32 = 16;

/// Blocks the destructive tests may touch, derived from the per-case layout
/// constants so growing any case grows the minimum automatically: the
/// single-block test writes [0, 8), the multi-block test [8, 16), the mixed
/// test [16, 24) and the multi-descriptor test [24, 8*2+8+16).
pub(crate) const SCRATCH_LAYOUT_BLOCKS: u32 =
    WRITE_TEST_BLOCKS * 2 + MIXED_TEST_BLOCKS + MULTI_DESCRIPTOR_TEST_BLOCKS;

/// The fixed scratch extent a dedicated marker disk must reserve:
/// [1, 1 + SCRATCH_LAYOUT_BLOCKS). LBA 0 stays untouched so the marker
/// itself remains readable.
pub(crate) fn marker_scratch_region() -> BlockRegion {
    BlockRegion::new(1, u64::from(SCRATCH_LAYOUT_BLOCKS))
}

/// Half-open extent disjointness: the regions share no LBA. Extents that
/// merely touch (one ends where the other starts) are disjoint.
pub(crate) fn regions_disjoint(left: BlockRegion, right: BlockRegion) -> bool {
    left.end_lba <= right.start_lba || right.end_lba <= left.start_lba
}

/// Decides whether the root disk may host the operator-declared scratch
/// region (`axtest.block_scratch=<start_lba>:<blocks>`).
///
/// `root_filesystem_region` is `None` for disks that are not the root
/// device, so only the root disk can qualify. `protected_regions` covers
/// every identified partition plus the partition-table metadata of the
/// disk; `None` means the layout is unknown (unregistered handle or failed
/// volume scan) and disqualifies the device. The extent must be writable,
/// address 512-byte sectors (the functional layouts' addressing unit), hold
/// at least [`SCRATCH_LAYOUT_BLOCKS`] blocks, fit inside the device, and
/// avoid the mounted root filesystem plus every protected region.
pub(crate) fn declared_scratch_device_eligible(
    info: DeviceInfo,
    scratch: BlockRegion,
    root_filesystem_region: Option<BlockRegion>,
    protected_regions: Option<&[BlockRegion]>,
) -> bool {
    let Some(protected_regions) = protected_regions else {
        return false;
    };
    !info.read_only
        && info.logical_block_size == 512
        && scratch.num_blocks() >= u64::from(SCRATCH_LAYOUT_BLOCKS)
        && scratch.end_lba <= info.num_blocks
        && root_filesystem_region.is_some_and(|root| regions_disjoint(scratch, root))
        && protected_regions
            .iter()
            .all(|&region| regions_disjoint(scratch, region))
}

/// Decides whether a disk qualifies as a dedicated marker scratch disk.
///
/// The fixed [`marker_scratch_region`] extent must fit the device, the
/// sector size must hold the first-sector marker, and no protected region
/// (partition or table metadata) may reach into the extent. The root disk,
/// read-only devices and disks with unknown layout never qualify; the
/// caller must additionally verify the marker bytes.
pub(crate) fn marker_scratch_device_eligible(
    info: DeviceInfo,
    is_root: bool,
    protected_regions: Option<&[BlockRegion]>,
) -> bool {
    let Some(protected_regions) = protected_regions else {
        return false;
    };
    let scratch = marker_scratch_region();
    !is_root
        && !info.read_only
        && info.num_blocks >= scratch.end_lba
        && info.logical_block_size >= SCRATCH_DISK_MARKER.len()
        && protected_regions
            .iter()
            .all(|&region| regions_disjoint(scratch, region))
}

/// Whether `scratch` holds a case that writes `[scratch.start + offset,
/// scratch.start + offset + blocks)`. Every destructive case checks its own
/// extent before the first write so growing a case layout without growing
/// the operator declaration fails the case instead of writing through the
/// reservation.
pub(crate) fn scratch_covers_case(
    scratch: BlockRegion,
    offset_blocks: u32,
    case_blocks: u32,
) -> bool {
    scratch.num_blocks() >= u64::from(offset_blocks) + u64::from(case_blocks)
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    fn writable_device(num_blocks: u64) -> DeviceInfo {
        DeviceInfo::new(num_blocks, 512)
    }

    // VisionFive 2 card layout: an MBR table with the rootfs at
    // [2099456, 124735488) and the region declared by
    // `axtest.block_scratch=2099200:256` just below it, sharing no LBA with
    // the filesystem. The MBR metadata is LBA 0 alone.
    const CARD_BLOCKS: u64 = 124_735_488;
    const MBR_METADATA: BlockRegion = BlockRegion::new(0, 1);
    const ROOT_FILESYSTEM: BlockRegion = BlockRegion::new(2_099_456, CARD_BLOCKS - 2_099_456);
    const DECLARED_SCRATCH: BlockRegion = BlockRegion::new(2_099_200, 256);

    #[test]
    fn declared_scratch_accepts_the_reserved_region_below_the_root_filesystem() {
        assert!(declared_scratch_device_eligible(
            writable_device(CARD_BLOCKS),
            DECLARED_SCRATCH,
            Some(ROOT_FILESYSTEM),
            Some(&[ROOT_FILESYSTEM, MBR_METADATA]),
        ));
    }

    #[test]
    fn declared_scratch_rejects_unknown_protected_regions() {
        // A missing region list means the disk layout is unknown, so neither
        // discovery path may ever select the device.
        assert!(!declared_scratch_device_eligible(
            writable_device(CARD_BLOCKS),
            DECLARED_SCRATCH,
            Some(ROOT_FILESYSTEM),
            None,
        ));
        assert!(!marker_scratch_device_eligible(
            writable_device(1_024),
            false,
            None,
        ));
    }

    #[test]
    fn declared_scratch_rejects_extents_shorter_than_the_test_layout() {
        // 16 blocks was accepted before the minimum-length check existed and
        // let the multi-descriptor test write past the declared reservation.
        let short = BlockRegion::new(2_099_200, 16);
        assert!(!declared_scratch_device_eligible(
            writable_device(CARD_BLOCKS),
            short,
            Some(ROOT_FILESYSTEM),
            Some(&[ROOT_FILESYSTEM, MBR_METADATA]),
        ));
        let missing_one = BlockRegion::new(2_099_200, u64::from(SCRATCH_LAYOUT_BLOCKS) - 1);
        assert!(!declared_scratch_device_eligible(
            writable_device(CARD_BLOCKS),
            missing_one,
            Some(ROOT_FILESYSTEM),
            Some(&[ROOT_FILESYSTEM, MBR_METADATA]),
        ));
        let exact = BlockRegion::new(2_099_200, u64::from(SCRATCH_LAYOUT_BLOCKS));
        assert!(declared_scratch_device_eligible(
            writable_device(CARD_BLOCKS),
            exact,
            Some(ROOT_FILESYSTEM),
            Some(&[ROOT_FILESYSTEM, MBR_METADATA]),
        ));
    }

    #[test]
    fn declared_scratch_rejects_extents_passing_the_device_end() {
        // The root filesystem ends at LBA 100 and the device ends exactly
        // with the declared extent.
        let root = BlockRegion::new(0, 100);
        let fitting = BlockRegion::new(100, 256);
        assert!(declared_scratch_device_eligible(
            writable_device(356),
            fitting,
            Some(root),
            Some(&[root]),
        ));
        // One block beyond the device must disqualify the extent.
        let overflowing = BlockRegion::new(101, 256);
        assert!(!declared_scratch_device_eligible(
            writable_device(356),
            overflowing,
            Some(root),
            Some(&[root]),
        ));
    }

    #[test]
    fn declared_scratch_must_not_overlap_the_root_filesystem_region() {
        let root = BlockRegion::new(1_000, 1_000);
        let overlapping = BlockRegion::new(900, 256);
        assert!(!declared_scratch_device_eligible(
            writable_device(4_096),
            overlapping,
            Some(root),
            Some(&[root]),
        ));
        // Extents adjacent to the filesystem share no LBA and stay allowed.
        let before = BlockRegion::new(744, 256);
        assert!(declared_scratch_device_eligible(
            writable_device(4_096),
            before,
            Some(root),
            Some(&[root]),
        ));
        let after = BlockRegion::new(2_000, 256);
        assert!(declared_scratch_device_eligible(
            writable_device(4_096),
            after,
            Some(root),
            Some(&[root]),
        ));
    }

    #[test]
    fn declared_scratch_must_not_overlap_other_partitions_on_the_root_disk() {
        // init_root mounts a /boot partition alongside the root filesystem,
        // so a scratch region inside it must be rejected even though it does
        // not overlap the root filesystem itself.
        let boot = BlockRegion::new(2_048, 2_048);
        let root = BlockRegion::new(1_048_576, CARD_BLOCKS - 1_048_576);
        let overlapping_boot = BlockRegion::new(1_024, 2_048);
        assert!(!declared_scratch_device_eligible(
            writable_device(CARD_BLOCKS),
            overlapping_boot,
            Some(root),
            Some(&[boot, root]),
        ));
        let adjacent = BlockRegion::new(0, 2_048);
        assert!(declared_scratch_device_eligible(
            writable_device(CARD_BLOCKS),
            adjacent,
            Some(root),
            Some(&[boot, root]),
        ));
    }

    #[test]
    fn declared_scratch_must_not_overlap_partition_table_metadata() {
        // A GPT disk keeps its header and entry areas before the first
        // usable LBA and behind the last one; a declaration that reaches
        // either side would destroy the table.
        let metadata = [
            BlockRegion::new(0, 34),
            BlockRegion::new(4_000, 96),
        ];
        let partition = BlockRegion::new(2_048, 1_952);
        let root = BlockRegion::new(2_048, 1_952);
        assert!(!declared_scratch_device_eligible(
            writable_device(4_096),
            BlockRegion::new(0, 256),
            Some(root),
            Some(&[partition, root, metadata[0], metadata[1]]),
        ));
        // The unallocated gap between the metadata areas is still safe.
        assert!(declared_scratch_device_eligible(
            writable_device(4_096),
            BlockRegion::new(100, 256),
            Some(root),
            Some(&[partition, root, metadata[0], metadata[1]]),
        ));
        // An MBR declaration covering LBA 0 destroys the partition table
        // sector even though it overlaps no partition.
        assert!(!declared_scratch_device_eligible(
            writable_device(CARD_BLOCKS),
            BlockRegion::new(0, 256),
            Some(ROOT_FILESYSTEM),
            Some(&[ROOT_FILESYSTEM, MBR_METADATA]),
        ));
    }

    #[test]
    fn declared_scratch_only_applies_to_the_writable_root_disk() {
        // A missing root filesystem region means the disk is not the root
        // device, so an operator declaration must never select it.
        assert!(!declared_scratch_device_eligible(
            writable_device(CARD_BLOCKS),
            DECLARED_SCRATCH,
            None,
            Some(&[]),
        ));
        let mut read_only = writable_device(CARD_BLOCKS);
        read_only.read_only = true;
        assert!(!declared_scratch_device_eligible(
            read_only,
            DECLARED_SCRATCH,
            Some(ROOT_FILESYSTEM),
            Some(&[ROOT_FILESYSTEM, MBR_METADATA]),
        ));
        let mut large_sector = writable_device(CARD_BLOCKS);
        large_sector.logical_block_size = 4_096;
        assert!(!declared_scratch_device_eligible(
            large_sector,
            DECLARED_SCRATCH,
            Some(ROOT_FILESYSTEM),
            Some(&[ROOT_FILESYSTEM, MBR_METADATA]),
        ));
    }

    #[test]
    fn marker_scratch_rejects_partitions_reaching_into_the_fixed_extent() {
        // The extent is [1, 41): a partition starting at LBA 40 shares LBA
        // 40 with the multi-descriptor test region and must disqualify the
        // disk. Comparing against the block count instead of the half-open
        // end LBA accepted this layout and corrupted the first partition
        // sector.
        let reaching_in = BlockRegion::new(40, 60);
        assert!(!marker_scratch_device_eligible(
            writable_device(1_024),
            false,
            Some(&[reaching_in]),
        ));
        // A partition starting exactly at the extent end shares no LBA.
        let after = BlockRegion::new(41, 59);
        assert!(marker_scratch_device_eligible(
            writable_device(1_024),
            false,
            Some(&[after]),
        ));
        // LBA 0 stays outside the extent, so a partition covering only the
        // boot sector is fine while one reaching LBA 1 is not.
        let boot_sector_only = BlockRegion::new(0, 1);
        assert!(marker_scratch_device_eligible(
            writable_device(1_024),
            false,
            Some(&[boot_sector_only]),
        ));
        let covering_start = BlockRegion::new(0, 2);
        assert!(!marker_scratch_device_eligible(
            writable_device(1_024),
            false,
            Some(&[covering_start]),
        ));
    }

    #[test]
    fn marker_scratch_rejects_a_detected_whole_disk_filesystem() {
        // A raw filesystem occupies the whole device even though it has no
        // partition entry. Its metadata and data must remain outside the
        // automatically selected marker extent.
        let raw_filesystem = BlockRegion::new(0, 1_024);
        assert!(!marker_scratch_device_eligible(
            writable_device(1_024),
            false,
            Some(&[raw_filesystem]),
        ));
    }

    #[test]
    fn marker_scratch_rejects_partition_table_metadata() {
        // On a GPT disk the primary header lives at LBA 1 and the entry
        // areas follow, so the fixed extent would overwrite the table even
        // though every partition starts far above it.
        let metadata = [BlockRegion::new(0, 34), BlockRegion::new(4_000, 96)];
        let partition = BlockRegion::new(2_048, 1_952);
        assert!(!marker_scratch_device_eligible(
            writable_device(4_096),
            false,
            Some(&[partition, metadata[0], metadata[1]]),
        ));
    }

    #[test]
    fn marker_scratch_requires_the_extent_to_fit_the_disk() {
        assert!(!marker_scratch_device_eligible(
            writable_device(40),
            false,
            Some(&[]),
        ));
        assert!(marker_scratch_device_eligible(
            writable_device(41),
            false,
            Some(&[]),
        ));
    }

    #[test]
    fn marker_scratch_rejects_root_readonly_and_small_sector_disks() {
        assert!(!marker_scratch_device_eligible(
            writable_device(1_024),
            true,
            Some(&[]),
        ));
        let mut read_only = writable_device(1_024);
        read_only.read_only = true;
        assert!(!marker_scratch_device_eligible(
            read_only,
            false,
            Some(&[]),
        ));
        // The marker must fit one logical sector.
        let tiny_sector = DeviceInfo::new(1_024, 16);
        assert!(!marker_scratch_device_eligible(
            tiny_sector,
            false,
            Some(&[]),
        ));
    }

    #[test]
    fn scratch_coverage_requires_the_whole_case_extent() {
        let scratch = BlockRegion::new(100, 40);
        assert!(scratch_covers_case(scratch, 0, 40));
        assert!(scratch_covers_case(scratch, 24, 16));
        assert!(!scratch_covers_case(scratch, 24, 17));
        assert!(!scratch_covers_case(scratch, 40, 1));
    }
}
