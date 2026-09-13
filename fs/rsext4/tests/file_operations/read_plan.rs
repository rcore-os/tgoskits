use rsext4::{
    bmalloc::{AbsoluteBN, InodeNumber},
    file::PreparedFileRead,
};

use super::*;

#[test]
fn prepared_read_matches_unaligned_data_and_preserves_eof_tail() {
    let contents: Vec<_> = (0..3 * BLOCK_SIZE + 13)
        .map(|index| (index % 251) as u8)
        .collect();
    let mut fixture = Fixture::new(&contents);
    let start = BLOCK_SIZE - 7;
    let mut output = vec![0xa5; contents.len()];
    let plan = fixture.prepare(start as u64..(start + output.len()) as u64);
    let mut reads = 0;
    let completed = plan
        .read(|block, bytes| {
            reads += 1;
            fixture
                .device
                .read_blocks(bytes, block, (bytes.len() / BLOCK_SIZE) as u32)
        })
        .unwrap();
    let len = completed
        .complete(&mut fixture.fs, &mut fixture.device, &mut output)
        .unwrap();
    assert_eq!(len, contents.len() - start);
    assert_eq!(&output[..len], &contents[start..]);
    assert!(output[len..].iter().all(|byte| *byte == 0xa5));
    assert_eq!(reads, 1, "contiguous uncached blocks were not batched");
}

#[test]
fn prepared_read_retains_dirty_snapshot_across_flush_and_eviction() {
    let mut fixture = Fixture::new(&vec![0x5a; 2 * BLOCK_SIZE]);
    let first = fixture.blocks[0];
    fixture
        .fs
        .datablock_cache
        .modify(&mut fixture.device, first, |bytes| bytes.fill(0xb6))
        .unwrap();
    let plan = fixture.prepare(0..(2 * BLOCK_SIZE) as u64);
    let mut reads = Vec::new();
    let completed = plan
        .read(|block, bytes| {
            // Flush and evict the dirty block while another block is being read.
            // The owned snapshot must not depend on a later cache lookup.
            fixture.fs.datablock_cache.flush_all(&mut fixture.device)?;
            fixture.fs.datablock_cache.invalidate(first);
            reads.push(block);
            fixture
                .device
                .read_blocks(bytes, block, (bytes.len() / BLOCK_SIZE) as u32)
        })
        .unwrap();
    let mut output = vec![0; 2 * BLOCK_SIZE];
    assert_eq!(
        completed
            .complete(&mut fixture.fs, &mut fixture.device, &mut output)
            .unwrap(),
        output.len()
    );
    assert_eq!(&output[..BLOCK_SIZE], &[0xb6; BLOCK_SIZE]);
    assert_eq!(&output[BLOCK_SIZE..], &[0x5a; BLOCK_SIZE]);
    assert_eq!(reads, [fixture.blocks[1]]);
}

#[test]
fn prepared_read_preserves_data_cache_over_journal_over_disk_priority() {
    let mut fixture = Fixture::new(&vec![0x5a; BLOCK_SIZE]);
    // Finish the fixture's creation transaction, then overlay the original disk
    // contents in the mounted journal without replacing its pending owners.
    fixture.fs.sync_filesystem(&mut fixture.device).unwrap();
    fixture.device.flush().unwrap();
    fixture
        .device
        .write_blocks(&[0xb6; BLOCK_SIZE], fixture.blocks[0], 1, true)
        .unwrap();
    for expected in [0xb6, 0xc7] {
        if expected == 0xc7 {
            fixture
                .fs
                .datablock_cache
                .modify_new(&mut fixture.device, fixture.blocks[0], |bytes| {
                    bytes.fill(expected)
                })
                .unwrap();
        }
        let plan = fixture.prepare(0..BLOCK_SIZE as u64);
        let completed = plan.read(|_, _| Err(Ext4Error::io())).unwrap();
        let mut output = [0; BLOCK_SIZE];
        assert_eq!(
            completed
                .complete(&mut fixture.fs, &mut fixture.device, &mut output)
                .unwrap(),
            BLOCK_SIZE
        );
        assert_eq!(output, [expected; BLOCK_SIZE]);
    }
}

#[test]
fn prepared_read_propagates_io_error_without_updating_atime() {
    let mut fixture = Fixture::new(b"hello");
    let before = fixture
        .fs
        .get_inode_by_num(&mut fixture.device, fixture.inode)
        .unwrap()
        .i_atime;
    let plan = fixture.prepare(0..5);
    let result = plan.read(|_, _| Err(Ext4Error::io()));
    assert!(matches!(result, Err(error) if error.kind() == Ext4Error::io().kind()));
    assert_eq!(
        fixture
            .fs
            .get_inode_by_num(&mut fixture.device, fixture.inode)
            .unwrap()
            .i_atime,
        before
    );
}

#[test]
fn prepared_read_zeroes_holes_and_unwritten_extents() {
    let mut fixture = Fixture::new(&vec![0x5a; 3 * BLOCK_SIZE]);
    let mut inode = fixture
        .fs
        .get_inode_by_num(&mut fixture.device, fixture.inode)
        .unwrap();
    // Build a leaf mapping with a hole at logical 1 and an unwritten block at 2.
    // Keep its original allocated blocks: this test isolates read semantics.
    inode.i_block = [0; 15];
    inode.i_block[0] = 0xf30a | (2 << 16);
    inode.i_block[1] = 4;
    inode.i_block[3] = 0;
    inode.i_block[4] = 1;
    inode.i_block[5] = fixture.blocks[0].to_u32().unwrap();
    inode.i_block[6] = 2;
    inode.i_block[7] = 0x8001;
    inode.i_block[8] = fixture.blocks[2].to_u32().unwrap();
    fixture
        .fs
        .modify_inode(&mut fixture.device, fixture.inode, |target| *target = inode)
        .unwrap();
    let plan = fixture.prepare(0..(3 * BLOCK_SIZE) as u64);
    let completed = plan
        .read(|block, bytes| {
            assert_eq!(block, fixture.blocks[0]);
            assert_eq!(bytes.len(), BLOCK_SIZE);
            fixture.device.read_blocks(bytes, block, 1)
        })
        .unwrap();
    let mut output = vec![0xa5; 3 * BLOCK_SIZE];
    completed
        .complete(&mut fixture.fs, &mut fixture.device, &mut output)
        .unwrap();
    assert_eq!(&output[..BLOCK_SIZE], &[0x5a; BLOCK_SIZE]);
    assert!(output[BLOCK_SIZE..].iter().all(|byte| *byte == 0));
}

#[test]
fn prepared_read_declines_legacy_mapping_without_changing_serialized_read() {
    let contents = vec![0x5a; 2 * BLOCK_SIZE];
    let mut fixture = Fixture::new(&contents);
    fixture
        .fs
        .modify_inode(&mut fixture.device, fixture.inode, |inode| {
            inode.i_flags &= !rsext4::disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            for (index, block) in fixture.blocks.iter().enumerate() {
                inode.i_block[index] = block.to_u32().unwrap();
            }
        })
        .unwrap();
    let mut output = vec![0; contents.len()];
    let copied = read_inode_data_into(
        &mut fixture.device,
        &mut fixture.fs,
        fixture.inode,
        0,
        &mut output,
    )
    .unwrap();
    assert_eq!(copied, contents.len());
    assert_eq!(output, contents);
    assert!(
        PreparedFileRead::prepare(
            &mut fixture.fs,
            &mut fixture.device,
            fixture.inode,
            0..contents.len() as u64
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn prepared_read_rejects_overlapping_logical_extents() {
    let mut fixture = Fixture::new(&vec![0x5a; 3 * BLOCK_SIZE]);
    fixture
        .fs
        .modify_inode(&mut fixture.device, fixture.inode, |inode| {
            inode.i_block = [0; 15];
            inode.i_block[0] = 0xf30a | (2 << 16);
            inode.i_block[1] = 4;
            inode.i_block[3] = 0;
            inode.i_block[4] = 2;
            inode.i_block[5] = fixture.blocks[0].to_u32().unwrap();
            inode.i_block[6] = 1;
            inode.i_block[7] = 1;
            inode.i_block[8] = fixture.blocks[2].to_u32().unwrap();
        })
        .unwrap();
    let result = PreparedFileRead::prepare(
        &mut fixture.fs,
        &mut fixture.device,
        fixture.inode,
        0..(3 * BLOCK_SIZE) as u64,
    );
    assert!(
        matches!(result, Err(error) if error.kind() == Ext4Error::corrupted().kind()),
        "overlapping file extents were accepted"
    );
}

#[test]
fn prepared_read_declines_large_requests_and_directories() {
    let mut fixture = Fixture::new(b"hello");
    assert!(
        PreparedFileRead::prepare(
            &mut fixture.fs,
            &mut fixture.device,
            fixture.inode,
            0..(1024 * 1024 + 1)
        )
        .unwrap()
        .is_none()
    );
    assert!(
        PreparedFileRead::prepare(
            &mut fixture.fs,
            &mut fixture.device,
            InodeNumber::new(2).unwrap(),
            0..1
        )
        .unwrap()
        .is_none()
    );
    let invalid = core::ops::Range { start: 7, end: 3 };
    assert!(
        matches!(PreparedFileRead::prepare(&mut fixture.fs, &mut fixture.device,
        fixture.inode, invalid), Err(error) if error.kind() == Ext4ErrorKind::InvalidInput)
    );
}

#[test]
fn prepared_read_at_eof_needs_no_io_or_atime_change() {
    let mut fixture = Fixture::new(b"hello");
    let before = fixture
        .fs
        .get_inode_by_num(&mut fixture.device, fixture.inode)
        .unwrap()
        .i_atime;
    for range in [0..0, 5..9] {
        let plan = fixture.prepare(range);
        let completed = plan.read(|_, _| Err(Ext4Error::io())).unwrap();
        let mut output = [0xa5; 4];
        assert_eq!(
            completed
                .complete(&mut fixture.fs, &mut fixture.device, &mut output)
                .unwrap(),
            0
        );
        assert_eq!(output, [0xa5; 4]);
    }
    assert_eq!(
        fixture
            .fs
            .get_inode_by_num(&mut fixture.device, fixture.inode)
            .unwrap()
            .i_atime,
        before
    );
}

struct Fixture {
    fs: Ext4FileSystem,
    device: Jbd2Dev<MockBlockDevice>,
    inode: InodeNumber,
    blocks: Vec<AbsoluteBN>,
}

impl Fixture {
    fn new(contents: &[u8]) -> Self {
        let mut device = Jbd2Dev::initial_jbd2dev(0, MockBlockDevice::new(32 * 1024 * 1024), true);
        mkfs(&mut device).unwrap();
        let mut fs = Ext4FileSystem::mount(&mut device).unwrap();
        mkfile(&mut device, &mut fs, "/input", Some(contents), None).unwrap();
        let (inode, mut node) = rsext4::dir::get_inode_with_num(&mut fs, &mut device, "/input")
            .unwrap()
            .unwrap();
        let blocks: Vec<_> =
            rsext4::loopfile::resolve_inode_blocks(&mut fs, &mut device, inode, &mut node)
                .unwrap()
                .into_values()
                .collect();
        fs.datablock_cache.flush_all(&mut device).unwrap();
        for block in &blocks {
            fs.datablock_cache.invalidate(*block);
        }
        Self {
            fs,
            device,
            inode,
            blocks,
        }
    }

    fn prepare(&mut self, range: core::ops::Range<u64>) -> PreparedFileRead {
        PreparedFileRead::prepare(&mut self.fs, &mut self.device, self.inode, range)
            .unwrap()
            .unwrap()
    }
}
