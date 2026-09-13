//! Journal checkpoint contracts.

use super::*;

#[test]
fn umount_commit_propagates_device_flush_failure() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");

    let error = dev
        .umount_commit()
        .expect_err("unmount commit must propagate the device error");

    assert_eq!(error, Ext4Error::io());
}

#[test]
fn flush_forces_pending_journal_transaction() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let target = AbsoluteBN::new(10);
    let payload = vec![0x5a; BLOCK_SIZE];
    let sequence = dev.journal_sequence().expect("journal sequence");
    dev.write_blocks(&payload, target, 1, true)
        .expect("queue metadata update");

    dev.flush().expect("flush pending transaction");

    assert_eq!(dev.journal_sequence(), Some(sequence.wrapping_add(1)));
    let inner = dev.into_inner();
    let start = target.as_usize().expect("target offset") * BLOCK_SIZE;
    assert_eq!(&inner.data[start..start + BLOCK_SIZE], payload);
}

#[test]
fn filesystem_sync_commits_without_checkpointing_home_metadata() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install checksummed journal");
    let target = AbsoluteBN::new(10);
    let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
    let payload = vec![0x73; BLOCK_SIZE];
    dev.write_blocks(&payload, target, 1, true)
        .expect("queue metadata update");

    dev.commit_for_filesystem_sync()
        .expect("commit filesystem sync transaction");

    assert!(
        dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0),
        "ordinary sync must not force committed metadata to its home block"
    );
    assert_eq!(
        dev.system
            .as_ref()
            .expect("journal state")
            .checkpoint_transactions
            .len(),
        1
    );
    assert_eq!(dev.inner._device().flush_calls, 1);
    assert_eq!(dev.inner._device().fua_writes, 1);

    dev.commit_for_filesystem_sync()
        .expect("clean filesystem sync");
    assert_eq!(
        dev.inner._device().flush_calls,
        2,
        "a sync without a transaction must still flush data writeback"
    );
}

#[test]
fn journal_state_cannot_be_reinstalled_with_pending_checkpoint_owner() {
    let superblock = csum_v3_superblock();
    let journal_start = AbsoluteBN::new(128);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(superblock, journal_start)
        .expect("install checksummed journal");
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");
    dev.commit().expect("commit metadata update");

    let error = dev
        .set_journal_superblock(superblock, journal_start)
        .expect_err("reinstall must not discard a pending checkpoint owner");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
    assert_eq!(
        dev.system
            .as_ref()
            .expect("journal state")
            .checkpoint_transactions
            .len(),
        1
    );
}

#[test]
fn commit_keeps_transaction_for_later_checkpoint() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let target = AbsoluteBN::new(10);
    let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
    let payload = vec![0x6b; BLOCK_SIZE];
    let sequence = dev.journal_sequence().expect("journal sequence");
    dev.write_blocks(&payload, target, 1, true)
        .expect("queue metadata update");

    dev.commit().expect("commit running transaction");

    assert_eq!(dev.journal_sequence(), Some(sequence.wrapping_add(1)));
    let system = dev.system.as_ref().expect("journal state");
    assert!(system.running_transaction.updates.is_empty());
    assert!(system.committing_transaction.is_none());
    assert_eq!(system.checkpoint_transactions.len(), 1);
    assert_ne!(
        system.jbd2_super_block.s_start, 0,
        "the oldest committed transaction must remain discoverable"
    );
    assert!(
        dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0),
        "commit must not synchronously checkpoint the home block"
    );

    let mut visible = vec![0; BLOCK_SIZE];
    dev.read_blocks(&mut visible, target, 1)
        .expect("read committed metadata through journal owner");
    assert_eq!(visible, payload);

    dev.flush().expect("checkpoint committed transaction");
    assert_eq!(
        &dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE],
        payload
    );
    assert_eq!(
        dev.system
            .as_ref()
            .expect("journal state")
            .jbd2_super_block
            .s_start,
        0,
        "checkpoint must reclaim the journal tail"
    );
}

#[test]
fn commit_record_uses_fua_after_descriptor_flush() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install checksummed journal");
    dev.write_blocks(&vec![0x67; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");

    dev.commit().expect("commit metadata transaction");

    assert_eq!(
        dev.inner._device().fua_writes,
        1,
        "the commit record must be the transaction's FUA publication"
    );
    assert_eq!(
        dev.inner._device().flush_calls,
        1,
        "the pre-commit flush orders descriptor and payload writes before the FUA commit"
    );
}

#[test]
fn checkpoint_reclaims_only_oldest_committed_transaction() {
    let superblock = csum_v3_superblock();
    let journal_start = AbsoluteBN::new(128);
    let first_target = AbsoluteBN::new(10);
    let second_target = AbsoluteBN::new(11);
    let first_payload = vec![0x51; BLOCK_SIZE];
    let second_payload = vec![0xa6; BLOCK_SIZE];
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(superblock, journal_start)
        .expect("install checksummed journal");

    dev.write_blocks(&first_payload, first_target, 1, true)
        .expect("queue first transaction");
    dev.commit().expect("commit first transaction");
    dev.write_blocks(&second_payload, second_target, 1, true)
        .expect("queue second transaction");
    dev.commit().expect("commit second transaction");

    dev.checkpoint_pending_transactions()
        .expect("checkpoint only the oldest transaction");

    let first_offset = first_target.as_usize().expect("first target") * BLOCK_SIZE;
    let second_offset = second_target.as_usize().expect("second target") * BLOCK_SIZE;
    assert_eq!(
        &dev.inner._device().data[first_offset..first_offset + BLOCK_SIZE],
        first_payload
    );
    assert!(
        dev.inner._device().data[second_offset..second_offset + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0),
        "a single checkpoint step must leave the later home image pending"
    );
    let system = dev.system.as_ref().expect("journal state");
    assert_eq!(system.checkpoint_transactions.len(), 1);
    assert_eq!(system.checkpoint_transactions[0].sequence, 2);
    assert_ne!(system.jbd2_super_block.s_start, 0);
    assert_eq!(system.jbd2_super_block.s_sequence, 2);

    let replay_superblock = system.jbd2_super_block;
    let inner = dev.into_inner();
    let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    replay_dev
        .set_journal_superblock(replay_superblock, journal_start)
        .expect("install advanced journal tail");
    assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);
    let inner = replay_dev.into_inner();
    assert_eq!(
        &inner.data[second_offset..second_offset + BLOCK_SIZE],
        second_payload,
        "the later committed transaction must remain replayable"
    );
}

#[test]
fn flush_batches_committed_transactions_into_one_tail_fua() {
    let superblock = csum_v3_superblock();
    let journal_start = AbsoluteBN::new(128);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(superblock, journal_start)
        .expect("install checksummed journal");

    dev.write_blocks(&vec![0x51; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue first transaction");
    dev.commit().expect("commit first transaction");
    dev.write_blocks(&vec![0xa6; BLOCK_SIZE], AbsoluteBN::new(11), 1, true)
        .expect("queue second transaction");
    dev.commit().expect("commit second transaction");
    assert_eq!(dev.inner._device().fua_writes, 2);

    dev.flush()
        .expect("checkpoint every committed transaction as one batch");

    assert_eq!(
        dev.inner._device().fua_writes,
        3,
        "one flush batch must publish the final journal tail with one FUA"
    );
    assert!(
        dev.system
            .as_ref()
            .expect("journal state")
            .checkpoint_transactions
            .is_empty()
    );
}

#[test]
fn checkpoint_batch_writes_only_latest_home_block_version() {
    let superblock = csum_v3_superblock();
    let journal_start = AbsoluteBN::new(128);
    let target = AbsoluteBN::new(10);
    let latest_payload = vec![0xa6; BLOCK_SIZE];
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(superblock, journal_start)
        .expect("install checksummed journal");

    dev.write_blocks(&vec![0x51; BLOCK_SIZE], target, 1, true)
        .expect("queue first transaction");
    dev.commit().expect("commit first transaction");
    dev.write_blocks(&latest_payload, target, 1, true)
        .expect("queue replacement transaction");
    dev.commit().expect("commit replacement transaction");
    dev.inner._device_mut().write_calls = 0;

    dev.flush()
        .expect("checkpoint every committed transaction as one batch");

    assert_eq!(
        dev.inner._device().write_calls,
        2,
        "checkpoint must write one latest home image and one tail superblock"
    );
    let target_offset = target.as_usize().expect("target") * BLOCK_SIZE;
    assert_eq!(
        &dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE],
        latest_payload
    );
}

#[test]
fn failed_tail_fua_preserves_oldest_replay_boundary() {
    let superblock = csum_v3_superblock();
    let journal_start = AbsoluteBN::new(128);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(superblock, journal_start)
        .expect("install checksummed journal");
    dev.write_blocks(&vec![0x37; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue first transaction");
    dev.commit().expect("commit first transaction");
    dev.write_blocks(&vec![0x92; BLOCK_SIZE], AbsoluteBN::new(11), 1, true)
        .expect("queue second transaction");
    dev.commit().expect("commit second transaction");

    let (tail_sequence, tail_start, used_records) = {
        let system = dev.system.as_ref().expect("journal state");
        (
            system.jbd2_super_block.s_sequence,
            system.jbd2_super_block.s_start,
            system.used_log_records,
        )
    };
    dev.inner._device_mut().fail_fua = true;

    let error = dev
        .checkpoint_pending_transactions()
        .expect_err("tail FUA failure must abort checkpoint");

    assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);
    let system = dev.system.as_ref().expect("journal state");
    assert_eq!(system.jbd2_super_block.s_sequence, tail_sequence);
    assert_eq!(system.jbd2_super_block.s_start, tail_start);
    assert_eq!(system.used_log_records, used_records);
    assert_eq!(system.checkpoint_transactions.len(), 2);
}

#[test]
fn partial_checkpoint_reuses_wrapped_log_without_losing_later_transactions() {
    let superblock = small_journal_superblock();
    let journal_start = AbsoluteBN::new(128);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(superblock, journal_start)
        .expect("install small journal");

    for transaction in 0..4u32 {
        dev.write_blocks(
            &vec![0x40 + transaction as u8; BLOCK_SIZE],
            AbsoluteBN::new(10 + u64::from(transaction)),
            1,
            true,
        )
        .expect("queue transaction before wrap");
        dev.commit().expect("commit transaction before wrap");
    }
    dev.checkpoint_pending_transactions()
        .expect("reclaim first transaction");
    dev.write_blocks(&vec![0x44; BLOCK_SIZE], AbsoluteBN::new(14), 1, true)
        .expect("queue transaction at ring end");
    dev.commit().expect("commit transaction at ring end");
    dev.checkpoint_pending_transactions()
        .expect("reclaim second transaction");
    dev.write_blocks(&vec![0x45; BLOCK_SIZE], AbsoluteBN::new(15), 1, true)
        .expect("queue wrapped transaction");
    dev.commit().expect("commit wrapped transaction");

    let system = dev.system.as_ref().expect("journal state");
    assert_eq!(system.checkpoint_transactions.len(), 4);
    assert_eq!(system.jbd2_super_block.s_sequence, 3);
    assert_eq!(system.jbd2_super_block.s_start, 7);
    assert_eq!(system.head, 4);
    let replay_superblock = system.jbd2_super_block;
    let inner = dev.into_inner();
    let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    replay_dev
        .set_journal_superblock(replay_superblock, journal_start)
        .expect("install partial-checkpoint tail");
    assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);
    let inner = replay_dev.into_inner();
    for transaction in 0..6u32 {
        let offset = (10 + transaction) as usize * BLOCK_SIZE;
        assert_eq!(
            inner.data[offset],
            0x40 + transaction as u8,
            "transaction {transaction} must survive checkpoint and replay"
        );
    }
}
