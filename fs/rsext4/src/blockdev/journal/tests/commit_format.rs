//! Journal commit format contracts.

use super::*;

#[test]
fn one_transaction_can_span_multiple_descriptor_blocks() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(2048), true);
    let mut superblock = JournalSuperBlock {
        s_maxlen: 1024,
        ..csum_v3_superblock()
    };
    crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
    dev.set_journal_superblock(superblock, AbsoluteBN::new(512))
        .expect("install journal with room for multiple descriptors");

    let target = AbsoluteBN::new(10);
    let payload_count = 255usize;
    dev.with_transaction_handle(payload_count, |device| {
        for index in 0..payload_count {
            let payload = vec![(index + 1) as u8; BLOCK_SIZE];
            device.write_blocks(
                &payload,
                target.checked_add(u32::try_from(index).unwrap()).unwrap(),
                1,
                true,
            )?;
        }
        Ok(())
    })
    .expect("one handle may exceed one descriptor's tag capacity");
    dev.umount_commit()
        .expect("commit multi-descriptor transaction");

    assert_eq!(dev.journal_sequence(), Some(2));
    let mut inner = dev.into_inner();
    let second_descriptor = &inner.data[767 * BLOCK_SIZE..768 * BLOCK_SIZE];
    let header = JournalHeaderS::from_disk_bytes(second_descriptor);
    assert_eq!(
        header.h_blocktype,
        crate::jbd2::jbdstruct::JBD2_BLOCKTYPE_DESCRIPTOR
    );
    assert_eq!(header.h_sequence, 1);
    let second_tag_offset = JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG3_SIZE + JBD2_UUID_SIZE;
    let second_tag = JournalBlockTag3S::from_disk_bytes(
        &second_descriptor[second_tag_offset..second_tag_offset + JBD2_TAG3_SIZE],
    );
    assert_ne!(
        second_tag.t_flags & u32::from(crate::jbd2::jbdstruct::JBD2_FLAG_LAST_TAG),
        0
    );

    for index in 0..payload_count {
        let block = target.checked_add(u32::try_from(index).unwrap()).unwrap();
        let start = block.as_usize().unwrap() * BLOCK_SIZE;
        inner.data[start..start + BLOCK_SIZE].fill(0);
    }
    let mut replay_superblock = superblock;
    replay_superblock.s_start = replay_superblock.s_first;
    replay_superblock.s_sequence = 1;
    crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
    replay_superblock.to_disk_bytes(&mut inner.data[512 * BLOCK_SIZE..][..1024]);

    let mut replay = Jbd2Dev::initial_jbd2dev(0, inner, true);
    replay
        .set_journal_superblock_with_mapping(
            replay_superblock,
            (512..1536).map(AbsoluteBN::new).collect(),
        )
        .expect("install committed multi-descriptor journal");
    assert_eq!(replay.journal_replay_checked(), ReplayStatus::Complete);
    let inner = replay.into_inner();
    for index in 0..payload_count {
        let block = target.checked_add(u32::try_from(index).unwrap()).unwrap();
        let start = block.as_usize().unwrap() * BLOCK_SIZE;
        assert!(
            inner.data[start..start + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == (index + 1) as u8)
        );
    }
}

#[test]
fn second_descriptor_write_failure_never_checkpoints_the_first_chunk() {
    let mut inner = MemBlockDev::new(2048);
    inner.fail_next_write_at_block(AbsoluteBN::new(767));
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    let mut superblock = JournalSuperBlock {
        s_maxlen: 1024,
        ..csum_v3_superblock()
    };
    crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
    dev.set_journal_superblock(superblock, AbsoluteBN::new(512))
        .expect("install journal with room for multiple descriptors");

    let target = AbsoluteBN::new(10);
    let payload_count = 255usize;
    dev.with_transaction_handle(payload_count, |device| {
        for index in 0..payload_count {
            let payload = vec![(index + 1) as u8; BLOCK_SIZE];
            device.write_blocks(
                &payload,
                target.checked_add(u32::try_from(index).unwrap()).unwrap(),
                1,
                true,
            )?;
        }
        Ok(())
    })
    .expect("queue one multi-descriptor transaction");

    let error = dev
        .umount_commit()
        .expect_err("second descriptor fault must abort before commit");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);
    let later = dev
        .write_block(AbsoluteBN::new(300), true)
        .expect_err("descriptor write fault must latch journal abort");
    assert_eq!(later.kind(), crate::Ext4ErrorKind::JournalAborted);

    let inner = dev.into_inner();
    for index in 0..payload_count {
        let block = target.checked_add(u32::try_from(index).unwrap()).unwrap();
        let start = block.as_usize().unwrap() * BLOCK_SIZE;
        assert!(
            inner.data[start..start + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0),
            "uncommitted descriptor prefix must not reach home block {block:?}"
        );
    }
}

#[test]
fn journal_install_rejects_ring_without_payload_capacity() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    let too_small = JournalSuperBlock {
        s_maxlen: 3,
        s_first: 1,
        ..JournalSuperBlock::default()
    };

    let error = dev
        .set_journal_superblock_with_mapping(too_small, (128..131).map(AbsoluteBN::new).collect())
        .expect_err("descriptor and commit alone leave no payload capacity");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
    assert_eq!(dev.journal_sequence(), None);
}

#[test]
fn csum_v3_commit_emits_linux_tag_and_block_checksums() {
    let (inner, superblock, target) = committed_csum_v3_fixture();
    let descriptor = &inner.data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
    let tag = JournalBlockTag3S::from_disk_bytes(
        &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE..JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG3_SIZE],
    );
    assert_eq!(tag.t_blocknr, target.raw() as u32);
    assert_eq!(tag.t_blocknr_high, 0);
    assert_eq!(
        tag.t_checksum,
        reference_jbd2_tag_checksum(
            &superblock.s_uuid,
            1,
            &inner.data[130 * BLOCK_SIZE..131 * BLOCK_SIZE],
        )
    );
    let descriptor_checksum = u32::from_be_bytes(descriptor[BLOCK_SIZE - 4..].try_into().unwrap());
    assert_eq!(
        descriptor_checksum,
        reference_jbd2_block_checksum(&superblock.s_uuid, descriptor, BLOCK_SIZE - 4,)
    );

    let commit_bytes = &inner.data[131 * BLOCK_SIZE..132 * BLOCK_SIZE];
    let commit = CommitHeader::from_disk_bytes(commit_bytes);
    assert_eq!(commit.h_chksum_type, 0);
    assert_eq!(commit.h_chksum_size, 0);
    assert_eq!(
        commit.h_chksum[0],
        reference_jbd2_block_checksum(&superblock.s_uuid, commit_bytes, 16)
    );
}

#[test]
fn commit_record_uses_the_injected_filesystem_clock() {
    let mut dev = Jbd2Dev::with_clock(0, MemBlockDev::new(256), FixedCommitClock, true);
    let superblock = csum_v3_superblock();
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .expect("install csum-v3 journal");
    dev.write_blocks(&vec![0xa5; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata");

    dev.umount_commit().expect("commit metadata");

    let inner = dev.into_inner();
    let commit = CommitHeader::from_disk_bytes(&inner.data[131 * BLOCK_SIZE..132 * BLOCK_SIZE]);
    assert_eq!(commit.h_commit_sec, 1_723_456_789);
    assert_eq!(commit.h_commit_nsec, 123_456_789);
}

#[test]
fn empty_commit_does_not_read_the_filesystem_clock() {
    let mut dev = Jbd2Dev::with_clock(0, MemBlockDev::new(256), FailingCommitClock, true);
    let superblock = csum_v3_superblock();
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .expect("install csum-v3 journal");

    dev.commit().expect("empty commit must not need time");
}

#[test]
fn commit_rejects_invalid_filesystem_time_before_switching_transaction_owner() {
    for timestamp in [
        Ext4Timestamp { sec: -1, nsec: 0 },
        Ext4Timestamp {
            sec: 1,
            nsec: Ext4Timestamp::MAX_NSEC + 1,
        },
    ] {
        let mut dev = Jbd2Dev::with_clock(
            0,
            MemBlockDev::new(256),
            InvalidCommitClock(timestamp),
            true,
        );
        let superblock = csum_v3_superblock();
        dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
            .expect("install csum-v3 journal");
        dev.write_blocks(&vec![0xa5; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue metadata");

        let error = dev
            .commit()
            .expect_err("invalid clock value must not reach a commit record");

        assert_eq!(error.kind(), crate::Ext4ErrorKind::InvalidInput);
        assert!(dev.journal_abort_cause().is_none());
        let system = dev.system.as_ref().expect("journal state");
        assert_eq!(system.running_transaction.updates.len(), 1);
        assert!(system.committing_transaction.is_none());
    }
}

#[test]
fn csum_v2_commit_emits_linux_tag_padding_and_block_checksums() {
    let (inner, superblock, target) = committed_csum_v2_fixture();
    let descriptor = &inner.data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
    let tag = JournalBlockTagS::from_disk_bytes(
        &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE..JBD2_DESCRIPTOR_HEADER_SIZE + 8],
    );
    assert_eq!(tag.t_blocknr, target.raw() as u32);
    assert_eq!(
        tag.t_checksum,
        reference_jbd2_tag_checksum(
            &superblock.s_uuid,
            1,
            &inner.data[130 * BLOCK_SIZE..131 * BLOCK_SIZE],
        ) as u16
    );
    assert_eq!(
        &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE + 8..JBD2_DESCRIPTOR_HEADER_SIZE + 10],
        &[0, 0],
        "Linux reserves two zero bytes in a 32-bit CSUM_V2 tag"
    );
    assert_eq!(
        &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE + 10..JBD2_DESCRIPTOR_HEADER_SIZE + 26],
        &superblock.s_uuid
    );
    let descriptor_checksum = u32::from_be_bytes(descriptor[BLOCK_SIZE - 4..].try_into().unwrap());
    assert_eq!(
        descriptor_checksum,
        reference_jbd2_block_checksum(&superblock.s_uuid, descriptor, BLOCK_SIZE - 4)
    );

    let commit_bytes = &inner.data[131 * BLOCK_SIZE..132 * BLOCK_SIZE];
    let commit = CommitHeader::from_disk_bytes(commit_bytes);
    assert_eq!(commit.h_chksum_type, 0);
    assert_eq!(commit.h_chksum_size, 0);
    assert_eq!(
        commit.h_chksum[0],
        reference_jbd2_block_checksum(&superblock.s_uuid, commit_bytes, 16)
    );
}

#[test]
fn csum_v2_64bit_tag_places_high_block_before_reserved_padding() {
    let (inner, superblock, target) =
        committed_csum_v2_fixture_with_features(JBD2_FEATURE_INCOMPAT_64BIT);
    let descriptor = &inner.data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
    let tag_offset = JBD2_DESCRIPTOR_HEADER_SIZE;
    let tag = JournalBlockTagS::from_disk_bytes(&descriptor[tag_offset..tag_offset + 8]);
    assert_eq!(tag.t_blocknr, target.raw() as u32);
    assert_eq!(
        u32::from_be_bytes(
            descriptor[tag_offset + 8..tag_offset + 12]
                .try_into()
                .expect("high block number")
        ),
        0
    );
    assert_eq!(&descriptor[tag_offset + 12..tag_offset + 14], &[0, 0]);
    assert_eq!(
        &descriptor[tag_offset + 14..tag_offset + 14 + JBD2_UUID_SIZE],
        &superblock.s_uuid
    );
}

#[test]
fn csum_v2_replay_accepts_valid_transaction_and_rejects_payload_corruption() {
    let (inner, superblock, target) = committed_csum_v2_fixture();
    let (status, replayed) = replay_csum_v2_fixture(inner, superblock, target);
    assert_eq!(status, ReplayStatus::Complete);
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &replayed.data[target_start..target_start + BLOCK_SIZE],
        vec![0x6d; BLOCK_SIZE]
    );

    let (mut corrupt, superblock, target) = committed_csum_v2_fixture();
    corrupt.data[130 * BLOCK_SIZE + 64] ^= 1;
    let (status, _) = replay_csum_v2_fixture(corrupt, superblock, target);
    let failure = status
        .failure()
        .expect("corrupt v2 payload must stop replay");
    assert_eq!(failure.phase(), JournalReplayPhase::Replay);
    assert_eq!(
        failure.cause().kind(),
        crate::Ext4ErrorKind::ChecksumMismatch
    );
}

fn assert_csum_v2_corruption_is_rejected(corrupt: impl FnOnce(&mut Vec<u8>)) {
    let (mut inner, superblock, target) = committed_csum_v2_fixture();
    corrupt(&mut inner.data);
    let (status, _) = replay_csum_v2_fixture(inner, superblock, target);
    let failure = status.failure().expect("v2 corruption must stop replay");
    assert_eq!(
        failure.cause().kind(),
        crate::Ext4ErrorKind::ChecksumMismatch
    );
}

#[test]
fn csum_v2_replay_rejects_descriptor_and_commit_corruption_before_home_write() {
    assert_csum_v2_corruption_is_rejected(|data| {
        data[130 * BLOCK_SIZE - 1] ^= 1;
    });
    assert_csum_v2_corruption_is_rejected(|data| {
        data[131 * BLOCK_SIZE + 16] ^= 1;
    });
}

#[test]
fn csum_v2_replay_rejects_corrupt_revoke_tail_before_home_write() {
    let (mut inner, superblock, target) = committed_csum_v2_fixture();
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

    let (status, _) = replay_csum_v2_fixture(inner, superblock, target);
    let failure = status
        .failure()
        .expect("corrupt v2 revoke must stop replay");
    assert_eq!(failure.phase(), JournalReplayPhase::Revoke);
    assert_eq!(
        failure.cause().kind(),
        crate::Ext4ErrorKind::ChecksumMismatch
    );
}

#[test]
fn compat_checksum_commit_covers_descriptor_and_payload_with_crc32_be() {
    let (inner, ..) = committed_compat_checksum_fixture();
    let descriptor = &inner.data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
    let payload = &inner.data[130 * BLOCK_SIZE..131 * BLOCK_SIZE];
    let commit = CommitHeader::from_disk_bytes(&inner.data[131 * BLOCK_SIZE..132 * BLOCK_SIZE]);
    let expected = reference_crc32_be(reference_crc32_be(u32::MAX, descriptor), payload);
    assert_eq!(commit.h_chksum_type, 1);
    assert_eq!(commit.h_chksum_size, 4);
    assert_eq!(commit.h_chksum[0], expected);
}

fn assert_compat_checksum_corruption_is_rejected(corrupt: impl FnOnce(&mut Vec<u8>)) {
    let (mut inner, superblock, target) = committed_compat_checksum_fixture();
    corrupt(&mut inner.data);
    let (status, _) = replay_compat_checksum_fixture(inner, superblock, target);
    let failure = status
        .failure()
        .expect("compat checksum mismatch must stop replay");
    assert_eq!(failure.phase(), JournalReplayPhase::Replay);
    assert_eq!(
        failure.cause().kind(),
        crate::Ext4ErrorKind::ChecksumMismatch
    );
}

#[test]
fn compat_checksum_replay_rejects_descriptor_payload_and_commit_corruption() {
    assert_compat_checksum_corruption_is_rejected(|data| {
        data[130 * BLOCK_SIZE - 1] ^= 1;
    });
    assert_compat_checksum_corruption_is_rejected(|data| {
        data[130 * BLOCK_SIZE + 64] ^= 1;
    });
    assert_compat_checksum_corruption_is_rejected(|data| {
        data[131 * BLOCK_SIZE + 16] ^= 1;
    });
}

#[test]
fn compat_checksum_replay_applies_a_valid_transaction() {
    let (inner, superblock, target) = committed_compat_checksum_fixture();
    let (status, replayed) = replay_compat_checksum_fixture(inner, superblock, target);
    assert_eq!(status, ReplayStatus::Complete);
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &replayed.data[target_start..target_start + BLOCK_SIZE],
        vec![0x93; BLOCK_SIZE]
    );
}
