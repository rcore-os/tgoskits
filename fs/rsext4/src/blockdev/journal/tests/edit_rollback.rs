//! Journal edit rollback contracts.

use super::*;

#[test]
fn active_journal_handle_rejects_commit_and_flush_without_state_change() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let target = AbsoluteBN::new(10);
    let sequence = dev.journal_sequence();

    dev.with_journal_handle(1, |dev| {
        assert_eq!(
            dev.system.as_ref().unwrap().running_transaction.phase,
            crate::jbd2::jbdstruct::Jbd2RunningTransactionPhase::Running
        );
        let error = dev
            .umount_commit()
            .expect_err("unmount cannot commit an active operation");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        assert_eq!(dev.journal_sequence(), sequence);
        let error = dev
            .flush()
            .expect_err("flush cannot commit an active operation");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        assert_eq!(dev.journal_sequence(), sequence);
        assert_eq!(
            dev.system.as_ref().unwrap().running_transaction.phase,
            crate::jbd2::jbdstruct::Jbd2RunningTransactionPhase::Running,
            "a rejected commit must not lock the active transaction"
        );
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], target, 1, true)
    })
    .expect("handle remains active after rejected unmount");

    assert_eq!(dev.journal_sequence(), sequence);
    dev.umount_commit().expect("commit after handle completion");
    let inner = dev.into_inner();
    let start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[start..start + BLOCK_SIZE],
        &vec![0x5a; BLOCK_SIZE]
    );
}
#[test]
fn umount_commit_returns_journal_superblock_write_failure_without_panicking() {
    let journal_superblock = AbsoluteBN::new(128);
    let mut inner = MemBlockDev::new(256);
    inner.fail_next_write_at_block(journal_superblock);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    dev.set_journal_superblock(small_journal_superblock(), journal_superblock)
        .expect("install journal state");
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dev.umount_commit()));

    assert!(result.is_ok(), "journal I/O failure must not panic");
    assert_eq!(result.unwrap(), Err(Ext4Error::io()));
}

#[test]
fn umount_commit_rejects_an_unfinished_edit_without_aborting_the_journal() {
    let cached_block = AbsoluteBN::new(20);
    let mut inner = MemBlockDev::new(256);
    inner.fail_next_write_at_block(cached_block);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install journal state");
    dev.read_block(cached_block).expect("prime cached block");
    dev.buffer_mut()[0] = 1;
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dev.umount_commit()));

    assert!(result.is_ok(), "an unfinished edit must not panic");
    assert_eq!(
        result.unwrap().unwrap_err().kind(),
        crate::Ext4ErrorKind::Busy
    );

    dev.inner.discard_held();
    dev.umount_commit()
        .expect("discarding the unpublished edit keeps the journal usable");
}

#[test]
fn commit_rejects_an_unfinished_block_edit_before_publishing_it_to_home() {
    let unfinished_block = AbsoluteBN::new(20);
    let journaled_block = AbsoluteBN::new(10);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install journal state");
    let sequence = dev.journal_sequence();

    dev.read_block(unfinished_block).expect("prime cache");
    dev.buffer_mut()[0] = 1;
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], journaled_block, 1, true)
        .expect("queue an unrelated journal update");

    let error = dev
        .commit()
        .expect_err("an unfinished edit must not bypass the journal");

    assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
    assert_eq!(dev.journal_sequence(), sequence);
    let offset = unfinished_block.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        dev.inner._device().data[offset],
        0,
        "the unfinished cache image must not reach the home block"
    );
}

#[test]
fn failed_block_edit_discards_the_unpublished_image() {
    let edited_block = AbsoluteBN::new(20);
    let journaled_block = AbsoluteBN::new(10);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install journal state");

    let error = dev
        .update_block(edited_block, true, |image| {
            image[0] = 1;
            Err::<(), _>(Ext4Error::io())
        })
        .expect_err("the edit closure must propagate its error");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], journaled_block, 1, true)
        .expect("queue an unrelated journal update");
    dev.umount_commit()
        .expect("the discarded edit must not block a later commit");

    let offset = edited_block.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        dev.inner._device().data[offset],
        0,
        "a failed edit must not reach the journal or home block"
    );
}

#[test]
fn failed_block_edit_publish_discards_the_unpublished_image() {
    let edited_block = AbsoluteBN::new(20);
    let mut inner = MemBlockDev::new(256);
    inner.fail_next_write_at_block(edited_block);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, false);

    let error = dev
        .update_block(edited_block, true, |image| {
            image[0] = 1;
            Ok(())
        })
        .expect_err("the direct publish failure must propagate");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

    dev.flush()
        .expect("the failed edit must not leave a dirty cache image");
    let offset = edited_block.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        dev.inner._device().data[offset],
        0,
        "a failed direct publish must not be retried by cache writeback"
    );
}

#[test]
fn rejects_an_empty_journal_mapping() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);

    let error = dev
        .set_journal_superblock_with_mapping(JournalSuperBlock::default(), Vec::new())
        .expect_err("empty journal mappings are corrupt");

    assert_eq!(error, Ext4Error::corrupted());
    assert_eq!(dev.journal_sequence(), None);
}

#[test]
fn direct_metadata_handle_restores_all_touched_home_blocks_on_error() {
    let first = AbsoluteBN::new(10);
    let second = AbsoluteBN::new(11);
    let first_before = vec![0x11; BLOCK_SIZE];
    let second_before = vec![0x22; BLOCK_SIZE];
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), false);
    dev.write_blocks(&first_before, first, 1, false)
        .expect("write first baseline");
    dev.write_blocks(&second_before, second, 1, false)
        .expect("write second baseline");

    let error = dev
        .with_transaction_handle(2, |dev| {
            dev.write_blocks(&vec![0xaa; BLOCK_SIZE], first, 1, true)?;
            dev.write_blocks(&vec![0xbb; BLOCK_SIZE], second, 1, true)?;
            Err::<(), _>(Ext4Error::io())
        })
        .expect_err("operation failure must abort direct metadata handle");
    assert_eq!(error, Ext4Error::io());

    let mut first_after = vec![0; BLOCK_SIZE];
    let mut second_after = vec![0; BLOCK_SIZE];
    dev.read_blocks(&mut first_after, first, 1)
        .expect("read restored first block");
    dev.read_blocks(&mut second_after, second, 1)
        .expect("read restored second block");
    assert_eq!(first_after, first_before);
    assert_eq!(second_after, second_before);
}

#[test]
fn direct_metadata_handle_credit_overrun_restores_earlier_write() {
    let first = AbsoluteBN::new(10);
    let second = AbsoluteBN::new(11);
    let first_before = vec![0x11; BLOCK_SIZE];
    let second_before = vec![0x22; BLOCK_SIZE];
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), false);
    dev.write_blocks(&first_before, first, 1, false)
        .expect("write first baseline");
    dev.write_blocks(&second_before, second, 1, false)
        .expect("write second baseline");

    let error = dev
        .with_transaction_handle(1, |dev| {
            dev.write_blocks(&vec![0xaa; BLOCK_SIZE], first, 1, true)?;
            dev.write_blocks(&vec![0xbb; BLOCK_SIZE], second, 1, true)
        })
        .expect_err("second distinct block must exceed direct handle credits");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);

    let mut first_after = vec![0; BLOCK_SIZE];
    let mut second_after = vec![0; BLOCK_SIZE];
    dev.read_blocks(&mut first_after, first, 1)
        .expect("read restored first block");
    dev.read_blocks(&mut second_after, second, 1)
        .expect("read untouched second block");
    assert_eq!(first_after, first_before);
    assert_eq!(second_after, second_before);
}
