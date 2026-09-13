//! Journal block io contracts.

use super::*;

#[test]
fn auto_commit_invalidates_stale_block_cache() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let sequence = dev.journal_sequence().unwrap();

    let target = AbsoluteBN::new(10);
    dev.read_block(target).expect("prime target cache");
    assert_eq!(dev.buffer()[0], 0);

    let count = u32::try_from(dev.journal_transaction_capacity().unwrap() + 1).unwrap();
    let mut updates = vec![0u8; count as usize * BLOCK_SIZE];
    for idx in 0..count as usize {
        updates[idx * BLOCK_SIZE] = (idx + 1) as u8;
    }

    dev.write_blocks(&updates, target, count, true)
        .expect("queue metadata updates");

    dev.read_block(target)
        .expect("read target after auto commit");
    assert_eq!(dev.buffer()[0], 1);
    assert_eq!(dev.journal_sequence(), Some(sequence.wrapping_add(1)));

    dev.umount_commit().expect("commit final queued update");
    assert_eq!(dev.journal_sequence(), Some(sequence.wrapping_add(2)));
    let inner = dev.into_inner();
    for idx in 0..count as usize {
        let start = (target.as_usize().unwrap() + idx) * BLOCK_SIZE;
        assert_eq!(inner.data[start], (idx + 1) as u8);
    }
}

#[test]
fn bulk_read_overlays_pending_journal_update() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");

    let target = AbsoluteBN::new(10);
    let pending = vec![0x5a; BLOCK_SIZE];
    dev.write_blocks(&pending, target, 1, true)
        .expect("queue metadata update");

    let mut observed = vec![0; BLOCK_SIZE];
    dev.read_blocks(&mut observed, target, 1)
        .expect("bulk read pending metadata");

    assert_eq!(observed, pending);
}

#[test]
fn single_block_read_uses_checkpoint_image_without_home_io() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .unwrap();
    let target = AbsoluteBN::new(10);
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], target, 1, true)
        .unwrap();
    dev.commit_pending_transaction().unwrap();
    dev.inner._device_mut().fail_next_read_at_block(target);

    let mut bytes = vec![0; BLOCK_SIZE];
    dev.read_blocks(&mut bytes, target, 1).unwrap();

    assert_eq!(bytes, vec![0x5a; BLOCK_SIZE]);
    assert!(dev.inner._device().fail_read_sector.is_some());
}

#[test]
fn single_block_read_honors_running_revoke_before_checkpoint_image() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .unwrap();
    let target = AbsoluteBN::new(10);
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], target, 1, true)
        .unwrap();
    dev.commit_pending_transaction().unwrap();
    dev.with_transaction_credits(TransactionCredits::metadata_with_revokes(0, 1), |device| {
        device.forget_detached_metadata(target)
    })
    .unwrap();
    dev.inner._device_mut().fail_next_read_at_block(target);

    let mut bytes = vec![0xee; BLOCK_SIZE];
    assert_eq!(
        dev.read_blocks(&mut bytes, target, 1).unwrap_err().kind(),
        crate::Ext4ErrorKind::Io
    );
    assert_eq!(bytes, vec![0xee; BLOCK_SIZE]);
}

#[test]
fn pending_image_cannot_bypass_device_range_validation() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .unwrap();
    let outside = AbsoluteBN::new(256);
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], outside, 1, true)
        .unwrap();

    let mut bytes = vec![0xee; BLOCK_SIZE];
    assert_eq!(
        dev.read_blocks(&mut bytes, outside, 1).unwrap_err(),
        Ext4Error::io().with_operation("device:sector_out_of_range")
    );
    assert_eq!(bytes, vec![0xee; BLOCK_SIZE]);
}

#[test]
fn metadata_write_never_bypasses_uninitialized_journal() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
    let target = AbsoluteBN::new(3);
    dev.read_block(target).expect("prime target buffer");
    dev.buffer_mut()[0] = 0x5a;

    let error = dev
        .write_block(target, true)
        .expect_err("metadata write must require initialized journal state");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);

    let inner = dev.into_inner();
    assert_eq!(inner.data[target.as_usize().unwrap() * BLOCK_SIZE], 0);
}

#[test]
fn unmount_commit_requires_initialized_journal() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
    let error = dev
        .umount_commit()
        .expect_err("journal-enabled unmount cannot claim a successful commit without state");

    assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
}

#[test]
fn abort_record_failure_does_not_replace_first_commit_error() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush_and_fua(256), true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install checksummed journal");
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");

    let first_error = dev
        .umount_commit()
        .expect_err("commit failure must remain the primary error");
    assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);
    let state = dev.abort_state.as_ref().expect("journal must be aborted");
    assert_eq!(state.cause.kind(), crate::Ext4ErrorKind::Io);
    assert_eq!(
        state.persistence_error.map(Ext4Error::kind),
        Some(crate::Ext4ErrorKind::Io)
    );
    assert_eq!(dev.inner._device().fua_writes, 1);

    let later_error = dev
        .umount_commit()
        .expect_err("the failed abort record must not allow a retry");
    assert_eq!(later_error.kind(), crate::Ext4ErrorKind::JournalAborted);

    let inner = dev.into_inner();
    let journal_offset = 128 * BLOCK_SIZE;
    let recorded =
        JournalSuperBlock::from_disk_bytes(&inner.data[journal_offset..journal_offset + 1024]);
    assert_eq!(
        recorded.s_errno, 0,
        "a failed FUA write must not claim the abort was recorded"
    );
}

#[test]
fn journal_superblock_must_match_filesystem_block_size() {
    let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
    let superblock = JournalSuperBlock {
        s_blocksize: 1024,
        ..Default::default()
    };

    let error = dev
        .validate_journal_superblock(&superblock, superblock.s_maxlen as usize)
        .expect_err("journal and filesystem block sizes must match");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::BadSuperblock);
}

#[test]
fn journal_v1_ignores_v2_extension_fields() {
    let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
    let superblock = JournalSuperBlock {
        s_header: crate::jbd2::jbdstruct::JournalHeaderS {
            h_blocktype: JBD2_BLOCKTYPE_SUPERBLOCK_V1,
            ..Default::default()
        },
        s_maxlen: 16,
        s_feature_compat: u32::MAX,
        s_feature_incompat: u32::MAX,
        s_feature_ro_compat: u32::MAX,
        s_checksum_type: u8::MAX,
        s_checksum: u32::MAX,
        ..Default::default()
    };

    dev.validate_journal_superblock(&superblock, 16)
        .expect("Linux ignores version-2 extension fields on a v1 journal");
}

#[test]
fn journal_v1_commits_without_interpreting_or_rewriting_v2_tail() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    let superblock = JournalSuperBlock {
        s_header: crate::jbd2::jbdstruct::JournalHeaderS {
            h_blocktype: JBD2_BLOCKTYPE_SUPERBLOCK_V1,
            ..Default::default()
        },
        s_maxlen: 16,
        s_feature_compat: u32::MAX,
        s_feature_incompat: u32::MAX,
        s_feature_ro_compat: u32::MAX,
        s_checksum_type: u8::MAX,
        s_checksum: 0xa5a5_5a5a,
        ..Default::default()
    };
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .unwrap();

    let target = AbsoluteBN::new(10);
    let payload = vec![0x5a; BLOCK_SIZE];
    dev.write_blocks(&payload, target, 1, true).unwrap();
    dev.umount_commit().unwrap();

    let inner = dev.into_inner();
    let home_offset = target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(&inner.data[home_offset..home_offset + BLOCK_SIZE], &payload);
    let journal_offset = 128 * BLOCK_SIZE;
    let persisted =
        JournalSuperBlock::decode_checked(&inner.data[journal_offset..journal_offset + BLOCK_SIZE])
            .unwrap();
    assert!(persisted.is_v1());
    assert_eq!(persisted.s_sequence, 2);
    assert_eq!(persisted.s_start, 0);
    assert_eq!(persisted.s_feature_incompat, u32::MAX);
    assert_eq!(persisted.s_checksum_type, u8::MAX);
    assert_eq!(persisted.s_checksum, 0xa5a5_5a5a);
}

#[test]
fn journal_superblock_checksum_is_verified_before_use() {
    let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
    let mut superblock = JournalSuperBlock::default();
    superblock.s_feature_incompat |= JBD2_FEATURE_INCOMPAT_CSUM_V3;
    superblock.s_checksum_type = JBD2_CRC32C_CHKSUM;
    crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
    superblock.s_checksum ^= 1;

    let error = dev
        .validate_journal_superblock(&superblock, superblock.s_maxlen as usize)
        .expect_err("damaged journal checksum must be rejected");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::ChecksumMismatch);
}

#[test]
fn journal_superblock_requires_block_checksum_feature_and_crc32c_together() {
    let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
    let mut missing_type = JournalSuperBlock::default();
    missing_type.s_feature_incompat |= JBD2_FEATURE_INCOMPAT_CSUM_V3;
    let error = dev
        .validate_journal_superblock(&missing_type, missing_type.s_maxlen as usize)
        .expect_err("csum-v3 requires CRC32C journal superblock checksums");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Unsupported);

    let mut missing_feature = JournalSuperBlock {
        s_checksum_type: JBD2_CRC32C_CHKSUM,
        ..Default::default()
    };
    crate::checksum::jbd2_update_superblock_checksum(&mut missing_feature);
    let error = dev
        .validate_journal_superblock(&missing_feature, missing_feature.s_maxlen as usize)
        .expect_err("CRC32C journal superblock checksums require csum-v3 support");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Unsupported);
}

#[test]
fn journal_superblock_accepts_linux_csum_v2_mode() {
    let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
    let mut superblock = JournalSuperBlock {
        s_maxlen: 16,
        s_feature_incompat: JBD2_FEATURE_INCOMPAT_CSUM_V2,
        s_checksum_type: JBD2_CRC32C_CHKSUM,
        s_uuid: [0x3c; JBD2_UUID_SIZE],
        ..Default::default()
    };
    crate::checksum::jbd2_update_superblock_checksum(&mut superblock);

    dev.validate_journal_superblock(&superblock, 16)
        .expect("Linux CSUM_V2 journal must be accepted");
}
