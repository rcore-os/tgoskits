//! Journal revoke contracts.

use super::*;

#[test]
fn later_writer_revoke_preserves_reused_home_block_after_replay() {
    let superblock = csum_v3_superblock();
    let journal_start = AbsoluteBN::new(128);
    let target = AbsoluteBN::new(10);
    let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
    let old_metadata = vec![0x41; BLOCK_SIZE];
    let new_owner = vec![0xb7; BLOCK_SIZE];
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(superblock, journal_start)
        .expect("install small journal");

    dev.write_blocks(&old_metadata, target, 1, true)
        .expect("queue old metadata image");
    dev.commit().expect("commit old metadata transaction");
    dev.with_transaction_credits(TransactionCredits::metadata_with_revokes(0, 1), |dev| {
        dev.forget_detached_metadata(target)?;
        dev.write_blocks(&new_owner, target, 1, false)
    })
    .expect("detach metadata and reuse its block in one transaction");
    dev.commit().expect("commit later revoke transaction");

    let inner = dev.into_inner();
    assert_eq!(
        &inner.data[target_offset..target_offset + BLOCK_SIZE],
        new_owner,
        "new owner must reach its home block before the revoke commit"
    );
    let revoke_offset = (journal_start.as_usize().expect("journal offset") + 4) * BLOCK_SIZE;
    let revoke_block = &inner.data[revoke_offset..revoke_offset + BLOCK_SIZE];
    let revoke = Jbd2JournalRevokeHeadS::from_disk_bytes(revoke_block);
    assert_eq!(revoke.r_header.h_blocktype, JBD2_BLOCKTYPE_REVOKE);
    assert_eq!(revoke.r_header.h_sequence, 2);
    assert_eq!(
        revoke.r_count, 24,
        "64-bit revoke must contain one u64 entry"
    );
    assert_eq!(
        u64::from_be_bytes(revoke_block[16..24].try_into().expect("revoke entry")),
        target.raw()
    );
    assert_eq!(
        u32::from_be_bytes(
            revoke_block[BLOCK_SIZE - 4..]
                .try_into()
                .expect("revoke checksum")
        ),
        reference_jbd2_block_checksum(&superblock.s_uuid, revoke_block, BLOCK_SIZE - 4)
    );

    let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    let mut replay_superblock = superblock;
    replay_superblock.s_start = replay_superblock.s_first;
    replay_superblock.s_sequence = 1;
    crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
    replay_dev
        .set_journal_superblock(replay_superblock, journal_start)
        .expect("restore pre-crash journal state");
    assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);

    let inner = replay_dev.into_inner();
    assert_eq!(
        &inner.data[target_offset..target_offset + BLOCK_SIZE],
        new_owner,
        "the later revoke must suppress replay of the detached metadata image"
    );
}

#[test]
fn later_writer_revoke_suppresses_older_checkpoint_write() {
    let superblock = csum_v3_superblock();
    let journal_start = AbsoluteBN::new(128);
    let target = AbsoluteBN::new(10);
    let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
    let old_metadata = vec![0x31; BLOCK_SIZE];
    let new_owner = vec![0xc4; BLOCK_SIZE];
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(superblock, journal_start)
        .expect("install checksummed journal");

    dev.write_blocks(&old_metadata, target, 1, true)
        .expect("queue old metadata image");
    dev.commit().expect("commit old metadata transaction");
    dev.with_transaction_credits(TransactionCredits::metadata_with_revokes(0, 1), |dev| {
        dev.forget_detached_metadata(target)?;
        dev.write_blocks(&new_owner, target, 1, false)
    })
    .expect("detach metadata and reuse its block");
    dev.commit().expect("commit revoke transaction");

    dev.flush().expect("checkpoint both committed transactions");

    assert_eq!(
        &dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE],
        new_owner,
        "checkpoint must skip an older image covered by a later revoke"
    );
    assert_eq!(
        dev.system
            .as_ref()
            .expect("journal state")
            .jbd2_super_block
            .s_start,
        0
    );
}

#[test]
fn metadata_reuse_cancels_same_transaction_revoke() {
    let superblock = csum_v3_superblock();
    let journal_start = AbsoluteBN::new(128);
    let target = AbsoluteBN::new(10);
    let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
    let old_metadata = vec![0x21; BLOCK_SIZE];
    let new_metadata = vec![0xd9; BLOCK_SIZE];
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(superblock, journal_start)
        .expect("install checksummed journal");

    dev.write_blocks(&old_metadata, target, 1, true)
        .expect("queue old metadata image");
    dev.commit().expect("commit old metadata transaction");
    dev.with_transaction_credits(TransactionCredits::metadata_with_revokes(1, 1), |dev| {
        dev.forget_detached_metadata(target)?;
        dev.write_blocks(&new_metadata, target, 1, true)
    })
    .expect("reuse metadata block in the running transaction");
    dev.commit().expect("commit replacement metadata");

    let inner = dev.into_inner();
    assert!(
        inner.data[target_offset..target_offset + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0),
        "metadata must remain journal-only before checkpoint"
    );
    let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    let mut replay_superblock = superblock;
    replay_superblock.s_start = replay_superblock.s_first;
    replay_superblock.s_sequence = 1;
    crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
    replay_dev
        .set_journal_superblock(replay_superblock, journal_start)
        .expect("restore pre-crash journal state");
    assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);

    let inner = replay_dev.into_inner();
    assert_eq!(
        &inner.data[target_offset..target_offset + BLOCK_SIZE],
        new_metadata,
        "journaling the reused block must cancel its same-transaction revoke"
    );
}

#[test]
fn existing_running_revoke_is_accounted_before_handle_reservation() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let sequence = dev.journal_sequence().expect("journal sequence");
    dev.forget_detached_metadata(AbsoluteBN::new(10))
        .expect("queue standalone revoke");

    let capacity = dev
        .journal_transaction_capacity()
        .expect("small journal capacity");
    dev.with_transaction_handle(capacity, |dev| {
        for offset in 0..capacity {
            let target = AbsoluteBN::new(
                20u64
                    .checked_add(u64::try_from(offset).map_err(|_| Ext4Error::overflow())?)
                    .ok_or_else(Ext4Error::overflow)?,
            );
            dev.write_blocks(&vec![offset as u8; BLOCK_SIZE], target, 1, true)?;
        }
        Ok(())
    })
    .expect("reserve a full metadata handle after closing the revoke transaction");

    assert_eq!(
        dev.journal_sequence(),
        Some(sequence.wrapping_add(1)),
        "the running revoke must be committed before the full handle starts"
    );
    dev.commit().expect("commit full metadata handle");
    dev.flush().expect("checkpoint both transactions");
    for offset in 0..capacity {
        let target_offset = (20 + offset) * BLOCK_SIZE;
        assert_eq!(
            &dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE],
            vec![offset as u8; BLOCK_SIZE]
        );
    }
}

#[test]
fn standalone_revoke_reserves_log_space_before_mutating_the_running_transaction() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    let constrained_ring = JournalSuperBlock {
        s_maxlen: 16,
        s_first: 13,
        ..JournalSuperBlock::default()
    };
    dev.set_journal_superblock(constrained_ring, AbsoluteBN::new(128))
        .expect("install journal whose ring is smaller than one maximum transaction");

    let error = dev
        .forget_detached_metadata(AbsoluteBN::new(10))
        .expect_err("revoke must not start without maximum-transaction log space");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
    assert!(
        dev.system
            .as_ref()
            .unwrap()
            .running_transaction
            .revoked_blocks
            .is_empty(),
        "failed start-time reservation must not publish a revoke"
    );
}
