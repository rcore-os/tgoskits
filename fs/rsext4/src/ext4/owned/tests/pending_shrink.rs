//! A shrink that published its size but failed to zero its EOF tail.
//!
//! The mount uses the shared-device cache, as an embedding kernel does, so a
//! failed write-through leaves the old bytes on the device and nothing dirty
//! in a private cache. Every way of growing the file again must still read
//! zero past the published size.

use core::cell::Cell;

use super::*;
use crate::{
    PreallocationOptions, RenameOptions,
    bmalloc::AbsoluteBN,
    jbd2::jbdstruct::{JBD2_BLOCKTYPE_SUPERBLOCK_V1, JournalSuperBlock},
};

std::thread_local! {
    static FAILED_SECTOR: Cell<Option<u64>> = const { Cell::new(None) };
}

const BLOCK: u64 = 4096;
/// Enough single-block extents for several external leaves under the inode
/// root, so that the shrink cannot fit one transaction of the small journal.
const FILE_BLOCKS: u64 = 650;
const KEPT: usize = 100;
const LATER_OFFSET: u64 = BLOCK + 500;
const LATER: &[u8] = b"later";
/// Forces the shrink to restart.
const SMALL_JOURNAL_BLOCKS: u32 = 30;
/// Holds the worst-case reservations of a rename over the file and of an
/// extent write that allocates.
const LATER_JOURNAL_BLOCKS: u32 = 256;

/// Fails the next device write that covers `sector`, once.
pub(super) fn observe_write(sector: SectorId, count: u32) -> Ext4Result<()> {
    FAILED_SECTOR.with(|failed| {
        let Some(target) = failed.get() else {
            return Ok(());
        };
        let start = sector.raw();
        if (start..start + u64::from(count)).contains(&target) {
            failed.set(None);
            return Err(Ext4Error::io());
        }
        Ok(())
    })
}

#[derive(Clone, Copy, Debug)]
enum Growth {
    DetachedWrite,
    ExtendingPreallocation,
}

#[derive(Clone, Copy, Debug)]
enum Name {
    Kept,
    Unlinked,
    RenamedOver,
}

#[test]
fn detached_write_after_failed_tail_zeroing_reads_zero_past_the_size() {
    assert_growth_reads_zero_past_the_size(Growth::DetachedWrite, Name::Kept);
}

#[test]
fn extending_preallocation_after_failed_tail_zeroing_reads_zero_past_the_size() {
    assert_growth_reads_zero_past_the_size(Growth::ExtendingPreallocation, Name::Kept);
}

#[test]
fn write_to_an_open_unlinked_file_after_failed_tail_zeroing_reads_zero_past_the_size() {
    assert_growth_reads_zero_past_the_size(Growth::DetachedWrite, Name::Unlinked);
}

#[test]
fn write_to_an_open_file_renamed_over_after_failed_tail_zeroing_reads_zero_past_the_size() {
    assert_growth_reads_zero_past_the_size(Growth::DetachedWrite, Name::RenamedOver);
}

fn assert_growth_reads_zero_past_the_size(growth: Growth, name: Name) {
    let (mut mount, number, _, first) = failed_tail_zeroing();
    let input = FileName::new(b"input").unwrap();
    match name {
        Name::Kept => {}
        Name::Unlinked => {
            let outcome = mount.unlink(mount.root_inode(), input).unwrap();
            assert_eq!(outcome.inode, number);
            assert!(outcome.requires_reap());
        }
        Name::RenamedOver => {
            let other = FileName::new(b"other").unwrap();
            mount
                .create_regular_file(
                    MutationContext::new(0, 0, 0, 0),
                    mount.root_inode(),
                    other,
                    FilePermissions::new(0o600).unwrap(),
                )
                .unwrap();
            let outcome = mount
                .rename(
                    mount.root_inode(),
                    other,
                    mount.root_inode(),
                    input,
                    RenameOptions::REPLACE,
                )
                .unwrap();
            let replaced = outcome.replaced.unwrap();
            assert_eq!(replaced.inode, number);
            assert!(replaced.requires_reap());
        }
    }

    let mut expected = first[..KEPT].to_vec();
    match growth {
        Growth::DetachedWrite => {
            let mut completed = mount
                .prepare_inode_write(number, LATER_OFFSET, LATER)
                .unwrap()
                .unwrap()
                .execute();
            mount.finish_inode_write(&mut completed).unwrap();
            expected.resize(LATER_OFFSET as usize, 0);
            expected.extend_from_slice(LATER);
        }
        Growth::ExtendingPreallocation => {
            mount
                .preallocate_inode(number, BLOCK, BLOCK, PreallocationOptions::EXTEND_SIZE)
                .unwrap();
            expected.resize(2 * BLOCK as usize, 0);
        }
    }
    assert_eq!(mount.inode(number).unwrap().size, expected.len() as u64);
    let mut output = alloc::vec![0xa5; expected.len()];
    assert_eq!(
        mount.read_inode(number, 0, &mut output).unwrap(),
        expected.len()
    );
    assert_eq!(&output[..KEPT], &expected[..KEPT]);
    assert_eq!(
        output
            .iter()
            .zip(&expected)
            .position(|(now, want)| now != want),
        None,
        "after {growth:?} with the name {name:?}, the bytes past the published size must be zero"
    );
    assert!(!mount.filesystem.pending_shrinks.contains(&number));
    if matches!(name, Name::Kept) {
        assert_eq!(mount.filesystem.superblock.s_last_orphan, 0);
    } else {
        mount.reap_unlinked_inode(number).unwrap();
        assert_eq!(mount.filesystem.superblock.s_last_orphan, 0);
    }
}

/// Shrinks the fragmented shared-device file to `KEPT` bytes on the restart
/// path and fails the write that zeroes its EOF tail after the first commit,
/// then installs a journal large enough for what follows. Returns the mount,
/// the inode, its retained EOF block and the original first block.
fn failed_tail_zeroing() -> (TestMount, InodeNumber, AbsoluteBN, Vec<u8>) {
    let (mut mount, number, first) = fragmented_shared_file();
    install_journal(&mut mount, SMALL_JOURNAL_BLOCKS);
    let mut inode = mount
        .filesystem
        .get_inode_by_num(&mut mount.device, number)
        .unwrap();
    let retained = crate::loopfile::resolve_inode_block(
        &mount.filesystem,
        &mut mount.device,
        number,
        &mut inode,
        0,
    )
    .unwrap()
    .unwrap();
    FAILED_SECTOR.with(|failed| failed.set(Some(retained.raw() * (BLOCK / 512))));
    assert_eq!(
        mount
            .truncate_inode(number, KEPT as u64)
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::Io
    );
    assert!(
        FAILED_SECTOR.with(Cell::get).is_none(),
        "the tail zeroing must have failed"
    );
    // The shrink published its size before zeroing the tail.
    assert_eq!(mount.inode(number).unwrap().size, KEPT as u64);
    assert!(mount.filesystem.pending_shrinks.contains(&number));
    install_journal(&mut mount, LATER_JOURNAL_BLOCKS);
    (mount, number, retained, first)
}

#[test]
fn failed_finish_of_a_published_shrink_keeps_the_tail_zero_on_the_device() {
    let (mut mount, number, retained, _) = failed_tail_zeroing();
    // Under-account the inode's blocks, so that finishing the shrink zeroes
    // the tail again and then fails inside its removal transaction.
    mount
        .filesystem
        .modify_inode(&mut mount.device, number, |inode| inode.i_blocks_lo = 8)
        .unwrap();
    let error = mount.truncate_inode(number, KEPT as u64).unwrap_err();
    assert_eq!(
        error.context(),
        Some(crate::error::ErrorContext::Operation {
            op: "extent:sub_inode_blocks"
        })
    );
    // The bytes past the published size are no longer part of the file, so
    // no read through the filesystem can show them. Only the device holds
    // the evidence: a failed finish must not write the old tail back.
    let mut block = alloc::vec![0xa5; BLOCK as usize];
    mount
        .device
        .read_blocks_uncached(&mut block, retained, 1)
        .unwrap();
    assert_eq!(
        block[KEPT..].iter().position(|byte| *byte != 0),
        None,
        "the retained block must stay zero past the published size"
    );
}

/// A shared-device file whose first block holds a pattern and whose later
/// blocks hold 0x5a, in single-block extents: each of its blocks is allocated
/// between two blocks of a spacer file.
fn fragmented_shared_file() -> (TestMount, InodeNumber, Vec<u8>) {
    let mut mount = mounted_filesystem();
    let number = create_file(&mut mount);
    let spacer = mount
        .create_regular_file(
            MutationContext::new(0, 0, 0, 0),
            mount.root_inode(),
            FileName::new(b"spacer").unwrap(),
            FilePermissions::new(0o600).unwrap(),
        )
        .unwrap()
        .number;
    mount.use_shared_device_cache().unwrap();
    let first: Vec<u8> = (0..BLOCK as usize)
        .map(|index| (index % 251) as u8 + 1)
        .collect();
    let filler = alloc::vec![0x5a; BLOCK as usize];
    for block in 0..FILE_BLOCKS {
        let contents = if block == 0 { &first } else { &filler };
        mount.write_inode(number, block * BLOCK, contents).unwrap();
        mount.write_inode(spacer, block * BLOCK, &filler).unwrap();
    }
    (mount, number, first)
}

/// Checkpoints the journal and installs a smaller one in its place.
fn install_journal(mount: &mut TestMount, maxlen: u32) {
    mount.filesystem.sync_filesystem(&mut mount.device).unwrap();
    mount.device.flush().unwrap();
    let start = mount.filesystem.journal_sb_block_start.unwrap();
    let mut journal = JournalSuperBlock::default();
    journal.s_header.h_blocktype = JBD2_BLOCKTYPE_SUPERBLOCK_V1;
    journal.s_blocksize = BLOCK as u32;
    journal.s_maxlen = maxlen;
    journal.s_first = 1;
    journal.s_sequence = 1;
    mount.device.set_journal_superblock(journal, start).unwrap();
}
