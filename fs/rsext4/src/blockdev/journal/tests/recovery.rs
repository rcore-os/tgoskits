//! Journal recovery contracts.

use super::*;

#[test]
fn csum_v3_commit_escapes_magic_without_changing_checkpoint_image() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    let superblock = csum_v3_superblock();
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .expect("install csum-v3 journal");
    let target = AbsoluteBN::new(10);
    let mut payload = vec![0x6b; BLOCK_SIZE];
    payload[..4].copy_from_slice(&JBD2_MAGIC.to_be_bytes());
    dev.write_blocks(&payload, target, 1, true)
        .expect("queue payload beginning with journal magic");

    dev.commit().expect("commit escaped payload");

    let descriptor = &dev.inner._device().data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
    let tag = JournalBlockTag3S::from_disk_bytes(
        &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE..JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG3_SIZE],
    );
    assert_ne!(tag.t_flags & u32::from(JOURNAL_ESCAPE), 0);
    let journal_payload = &dev.inner._device().data[130 * BLOCK_SIZE..131 * BLOCK_SIZE];
    assert_eq!(&journal_payload[..4], &[0; 4]);
    assert_eq!(&journal_payload[4..], &payload[4..]);
    assert_eq!(
        tag.t_checksum,
        reference_jbd2_tag_checksum(&superblock.s_uuid, 1, journal_payload)
    );
    assert_eq!(
        &dev.system
            .as_ref()
            .expect("journal state")
            .checkpoint_transactions[0]
            .updates[0]
            .1[..],
        payload,
        "checkpoint must retain the unescaped home image"
    );
}

#[test]
fn csum_v3_writer_transaction_replays_after_checkpoint_loss() {
    let (inner, superblock, target) = committed_csum_v3_fixture();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    dev.set_journal_superblock_with_mapping(superblock, (128..192).map(AbsoluteBN::new).collect())
        .expect("install writer-produced csum-v3 journal");

    assert_eq!(dev.journal_replay_checked(), ReplayStatus::Complete);
    let inner = dev.into_inner();
    assert!(
        inner.data[target_start..target_start + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0xa5)
    );
}

#[test]
fn replay_descriptor_read_failure_preserves_io_cause() {
    let (mut inner, superblock, _) = committed_csum_v3_fixture();
    inner.fail_next_read_at_block(AbsoluteBN::new(129));
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    dev.set_journal_superblock_with_mapping(superblock, (128..192).map(AbsoluteBN::new).collect())
        .expect("install writer-produced csum-v3 journal");

    let failure = dev
        .journal_replay_checked()
        .failure()
        .expect("replay must stop when the descriptor cannot be read");
    assert_eq!(failure.phase(), JournalReplayPhase::Scan);
    assert_eq!(failure.restart_rel(), Some(superblock.s_first));
    assert_eq!(failure.cause().kind(), crate::Ext4ErrorKind::Io);
    assert_eq!(failure.persistence_error(), None);
    let state = dev.abort_state.as_ref().expect("replay must abort journal");
    assert_eq!(
        state.cause.kind(),
        crate::Ext4ErrorKind::Io,
        "device I/O must not be collapsed into corruption"
    );
}

#[test]
fn replay_payload_read_and_home_write_failures_keep_replay_phase() {
    let (mut inner, superblock, _) = committed_csum_v3_fixture();
    inner.fail_next_read_at_block(AbsoluteBN::new(130));
    let mut read_failure_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    read_failure_dev
        .set_journal_superblock_with_mapping(superblock, (128..192).map(AbsoluteBN::new).collect())
        .expect("install replay fixture for payload read fault");

    let read_failure = read_failure_dev
        .journal_replay_checked()
        .failure()
        .expect("payload read fault must stop replay");
    assert_eq!(read_failure.phase(), JournalReplayPhase::Replay);
    assert_eq!(read_failure.cause().kind(), crate::Ext4ErrorKind::Io);

    let (mut inner, superblock, target) = committed_csum_v3_fixture();
    inner.fail_next_write_at_block(target);
    let mut write_failure_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    write_failure_dev
        .set_journal_superblock_with_mapping(superblock, (128..192).map(AbsoluteBN::new).collect())
        .expect("install replay fixture for home write fault");

    let write_failure = write_failure_dev
        .journal_replay_checked()
        .failure()
        .expect("home write fault must stop replay");
    assert_eq!(write_failure.phase(), JournalReplayPhase::Replay);
    assert_eq!(write_failure.cause().kind(), crate::Ext4ErrorKind::Io);
    let inner = write_failure_dev.into_inner();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert!(
        inner.data[target_start..target_start + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0)
    );
}

#[test]
fn replay_persist_failure_is_typed_after_successful_home_write() {
    let (mut inner, superblock, target) = committed_csum_v3_fixture();
    inner.fail_flush = true;
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    dev.set_journal_superblock_with_mapping(superblock, (128..192).map(AbsoluteBN::new).collect())
        .expect("install replay fixture with persist fault");

    let failure = dev
        .journal_replay_checked()
        .failure()
        .expect("final replay flush fault must fail recovery");
    assert_eq!(failure.phase(), JournalReplayPhase::Persist);
    assert_eq!(failure.cause().kind(), crate::Ext4ErrorKind::Io);
    assert_eq!(failure.persistence_error(), None);
    let inner = dev.into_inner();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert!(
        inner.data[target_start..target_start + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0xa5)
    );
}

#[test]
fn replay_superblock_write_failure_is_a_persist_error() {
    let (mut inner, superblock, target) = committed_csum_v3_fixture();
    inner.fail_next_write_at_block(AbsoluteBN::new(128));
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    dev.set_journal_superblock_with_mapping(superblock, (128..192).map(AbsoluteBN::new).collect())
        .expect("install replay fixture with superblock write fault");

    let failure = dev
        .journal_replay_checked()
        .failure()
        .expect("replay superblock write fault must fail recovery");
    assert_eq!(failure.phase(), JournalReplayPhase::Persist);
    assert_eq!(failure.cause().kind(), crate::Ext4ErrorKind::Io);
    assert_eq!(failure.persistence_error(), None);
    assert_eq!(dev.inner._device().fua_writes, 1);

    let inner = dev.into_inner();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert!(
        inner.data[target_start..target_start + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0xa5)
    );
}

#[test]
fn replay_failure_keeps_primary_checksum_over_progress_flush_error() {
    let (mut inner, superblock, target) = committed_csum_v3_fixture();
    inner.data[130 * BLOCK_SIZE - 1] ^= 1;
    inner.fail_flush = true;
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    dev.set_journal_superblock_with_mapping(superblock, (128..192).map(AbsoluteBN::new).collect())
        .expect("install corrupt replay fixture with persist fault");

    let failure = dev
        .journal_replay_checked()
        .failure()
        .expect("checksum and persist faults must fail replay");
    assert_eq!(failure.phase(), JournalReplayPhase::Scan);
    assert_eq!(
        failure.cause().kind(),
        crate::Ext4ErrorKind::ChecksumMismatch
    );
    assert_eq!(
        failure.persistence_error().map(Ext4Error::kind),
        Some(crate::Ext4ErrorKind::Io)
    );
    let state = dev.abort_state.as_ref().expect("replay must abort journal");
    assert_eq!(state.cause.kind(), crate::Ext4ErrorKind::ChecksumMismatch);
    assert_eq!(state.replay_failure, Some(failure));

    let inner = dev.into_inner();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert!(
        inner.data[target_start..target_start + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0)
    );
}

fn assert_csum_v3_corruption_is_rejected(corrupt: impl FnOnce(&mut Vec<u8>)) {
    let (mut inner, superblock, target) = committed_csum_v3_fixture();
    corrupt(&mut inner.data);
    let (status, _) = replay_csum_v3_fixture(inner, superblock, target);
    let failure = status.failure().expect("corruption must stop replay");
    assert_eq!(
        failure.cause().kind(),
        crate::Ext4ErrorKind::ChecksumMismatch
    );
}

#[test]
fn csum_v3_replay_rejects_descriptor_payload_and_commit_corruption_before_home_write() {
    assert_csum_v3_corruption_is_rejected(|data| {
        data[130 * BLOCK_SIZE - 1] ^= 1;
    });
    assert_csum_v3_corruption_is_rejected(|data| {
        data[130 * BLOCK_SIZE + 64] ^= 1;
    });
    assert_csum_v3_corruption_is_rejected(|data| {
        data[131 * BLOCK_SIZE + 16] ^= 1;
    });
}

#[test]
fn csum_v3_replay_accepts_partial_commit_block_checksum() {
    let (mut inner, superblock, target) = committed_csum_v3_fixture();
    inner.data[131 * BLOCK_SIZE + JBD2_COMMIT_HEADER_SIZE] = 0x7e;

    let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    replay_dev
        .set_journal_superblock_with_mapping(superblock, (128..192).map(AbsoluteBN::new).collect())
        .expect("install csum-v3 journal");

    assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);
    let inner = replay_dev.into_inner();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[target_start..target_start + BLOCK_SIZE],
        vec![0xa5; BLOCK_SIZE]
    );
}

#[test]
fn csum_v3_replay_rejects_corrupt_revoke_tail_before_home_write() {
    let (mut inner, superblock, target) = committed_csum_v3_fixture();
    inner
        .data
        .copy_within(131 * BLOCK_SIZE..132 * BLOCK_SIZE, 132 * BLOCK_SIZE);

    let mut revoke = vec![0u8; BLOCK_SIZE];
    Jbd2JournalRevokeHeadS {
        r_header: JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_REVOKE,
            h_sequence: 1,
        },
        r_count: 16,
    }
    .to_disk_bytes(&mut revoke);
    let checksum = crate::checksum::jbd2_descriptor_block_csum32(&superblock.s_uuid, &revoke)
        .expect("revoke checksum");
    revoke[BLOCK_SIZE - 4..].copy_from_slice(&checksum.to_be_bytes());
    revoke[BLOCK_SIZE - 1] ^= 1;
    inner.data[131 * BLOCK_SIZE..132 * BLOCK_SIZE].copy_from_slice(&revoke);

    let (status, _) = replay_csum_v3_fixture(inner, superblock, target);
    let failure = status.failure().expect("corrupt revoke must stop replay");
    assert_eq!(failure.phase(), JournalReplayPhase::Revoke);
    assert_eq!(
        failure.cause().kind(),
        crate::Ext4ErrorKind::ChecksumMismatch
    );
}

#[test]
fn csum_v3_replay_validates_all_payloads_before_any_home_write() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    let superblock = csum_v3_superblock();
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .expect("install csum-v3 journal");
    let first_target = AbsoluteBN::new(10);
    let second_target = AbsoluteBN::new(11);
    dev.write_blocks(&vec![0xa5; BLOCK_SIZE * 2], first_target, 2, true)
        .expect("queue two csum-v3 metadata blocks");
    dev.umount_commit().expect("commit csum-v3 metadata");

    let mut inner = dev.into_inner();
    let first_home = first_target.as_usize().unwrap() * BLOCK_SIZE;
    let second_home = second_target.as_usize().unwrap() * BLOCK_SIZE;
    inner.data[first_home..first_home + BLOCK_SIZE].fill(0);
    inner.data[second_home..second_home + BLOCK_SIZE].fill(0);
    inner.data[131 * BLOCK_SIZE + 64] ^= 1;
    let mut replay_superblock = superblock;
    replay_superblock.s_start = replay_superblock.s_first;
    replay_superblock.s_sequence = 1;
    crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
    replay_superblock.to_disk_bytes(&mut inner.data[128 * BLOCK_SIZE..][..1024]);

    let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    replay_dev
        .set_journal_superblock_with_mapping(
            replay_superblock,
            (128..192).map(AbsoluteBN::new).collect(),
        )
        .expect("install csum-v3 journal");
    assert!(replay_dev.journal_replay_checked().failure().is_some());
    let inner = replay_dev.into_inner();
    assert!(
        inner.data[first_home..first_home + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0)
    );
    assert!(
        inner.data[second_home..second_home + BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0)
    );
}
