//! Journal credits contracts.

use super::*;

#[test]
fn transaction_capacity_reserves_linux_third_of_log_and_bookkeeping() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    let superblock = csum_v3_superblock();
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .expect("install csum-v3 journal");

    assert_eq!(dev.journal_maximum_transaction_records().unwrap(), 21);
    assert_eq!(dev.journal_transaction_capacity().unwrap(), 19);
    let large_ring = JournalSuperBlock {
        s_maxlen: 4096,
        ..superblock
    };
    assert_eq!(
        Jbd2Dev::<MemBlockDev>::transaction_capacity(&large_ring, 1024, 4096).unwrap(),
        1341
    );

    let small = small_journal_superblock();
    assert_eq!(
        Jbd2Dev::<MemBlockDev>::transaction_capacity(&small, BLOCK_SIZE, 16).unwrap(),
        3
    );
}

#[test]
fn reserved_handle_is_limited_to_half_the_user_transaction_capacity() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install csum-v3 journal");
    assert_eq!(dev.journal_transaction_capacity().unwrap(), 19);
    let mut operation_started = false;

    let error = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(10),
            |_| {
                operation_started = true;
                Ok(())
            },
        )
        .expect_err("Linux limits journal-wide reservations to half the user capacity");

    assert!(!operation_started, "the rejected operation must not start");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
    assert_eq!(
        error.context(),
        Some(crate::ErrorContext::Operation {
            op: "jbd2:reserved_credits"
        })
    );
}

#[test]
fn bulk_block_byte_count_reports_overflow_without_panicking() {
    let result = std::panic::catch_unwind(|| checked_block_bytes(usize::MAX, 2));
    let bytes = result.expect("checked byte-count arithmetic must not panic");
    assert_eq!(bytes.unwrap_err().kind(), crate::Ext4ErrorKind::Overflow);
}

#[test]
fn reserved_handle_attaches_without_switching_the_running_transaction() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let first = AbsoluteBN::new(10);
    let second = AbsoluteBN::new(11);

    let ((), reserved) = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(1),
            |dev| dev.write_blocks(&vec![0x31; BLOCK_SIZE], first, 1, true),
        )
        .expect("parent handle and reservation fit the transaction");
    let sequence = dev.journal_sequence().expect("journal sequence");

    dev.with_reserved_transaction(reserved, |dev| {
        assert_eq!(
            dev.journal_sequence(),
            Some(sequence),
            "starting a reserved handle must not commit or switch transactions"
        );
        dev.write_blocks(&vec![0x42; BLOCK_SIZE], second, 1, true)
    })
    .expect("attach reserved handle");
    assert_eq!(dev.journal_sequence(), Some(sequence));

    dev.commit().expect("commit shared transaction");
    assert_eq!(
        dev.system.as_ref().unwrap().checkpoint_transactions[0]
            .updates
            .len(),
        2
    );
}

#[test]
fn detached_reservation_is_counted_before_an_ordinary_handle_starts() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");

    let ((), reserved) = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(1),
            |dev| dev.write_blocks(&vec![0x51; BLOCK_SIZE], AbsoluteBN::new(10), 1, true),
        )
        .expect("create detached reservation");
    let sequence = dev.journal_sequence().expect("journal sequence");

    dev.with_transaction_handle(2, |dev| {
        assert_ne!(
            dev.journal_sequence(),
            Some(sequence),
            "the earlier transaction must commit before credits can overlap the reservation"
        );
        Ok(())
    })
    .expect("ordinary handle starts after capacity is reclaimed");
    dev.free_reserved_transaction(reserved)
        .expect("release unused reservation");
}

#[test]
fn failed_parent_handle_releases_its_reserved_credits() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install csum-v3 journal");

    let error = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(9),
            |_| Err::<(), _>(Ext4Error::io().with_operation("test:reserved_parent_abort")),
        )
        .expect_err("parent failure must not publish its reserved handle");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);
    assert!(dev.reserved_handles.is_empty());

    let ((), reserved) = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(9),
            |_| Ok(()),
        )
        .expect("the full half-transaction reservation must be available again");
    dev.free_reserved_transaction(reserved)
        .expect("release replacement reservation");
    assert!(dev.reserved_handles.is_empty());
}

#[test]
fn journal_wide_reserved_credits_do_not_exceed_half_the_capacity() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install csum-v3 journal");

    let ((), first) = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(5),
            |_| Ok(()),
        )
        .expect("first reservation fits");
    let mut operation_started = false;
    let error = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(5),
            |_| {
                operation_started = true;
                Ok(())
            },
        )
        .expect_err("the aggregate reservation exceeds half the capacity");
    assert!(!operation_started);
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
    dev.free_reserved_transaction(first)
        .expect("release first reservation");
}

#[test]
fn failed_start_reserved_consumes_the_detached_token() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install csum-v3 journal");
    let ((), reserved) = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(1),
            |_| Ok(()),
        )
        .expect("create detached reservation");
    dev.abort_journal(Ext4Error::io().with_operation("test:abort_before_start_reserved"));

    let error = dev
        .with_reserved_transaction(reserved, |_| Ok(()))
        .expect_err("start-reserved must report the sticky abort");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
    assert!(
        dev.reserved_handles.is_empty(),
        "a failed start-reserved consumes and frees the token"
    );
}

#[test]
fn detached_reserved_handle_must_be_resolved_before_unmount() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install csum-v3 journal");
    let ((), reserved) = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(1),
            |_| Ok(()),
        )
        .expect("create detached reservation");

    let error = dev
        .umount_commit()
        .expect_err("unmount cannot discard a live reservation");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
    assert_eq!(
        error.context(),
        Some(crate::ErrorContext::Operation {
            op: "jbd2:unmount_with_reserved_handle"
        })
    );
    dev.free_reserved_transaction(reserved)
        .expect("release reservation");
    dev.umount_commit().expect("unmount after explicit release");
}

#[test]
fn revoke_records_consume_descriptor_credits_instead_of_metadata_credits() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(4096), true);
    let superblock = csum_v3_superblock();
    dev.set_journal_superblock(superblock, AbsoluteBN::new(2048))
        .expect("install csum-v3 journal");
    let revoke_records_per_block =
        (BLOCK_SIZE - core::mem::size_of::<Jbd2JournalRevokeHeadS>() - core::mem::size_of::<u32>())
            / core::mem::size_of::<u64>();
    let metadata_target = AbsoluteBN::new(1536);

    dev.with_transaction_credits(
        TransactionCredits::metadata_with_revokes(1, revoke_records_per_block),
        |dev| {
            for index in 0..revoke_records_per_block {
                dev.forget_detached_metadata(AbsoluteBN::new(100 + index as u64))?;
            }
            dev.write_blocks(&vec![0x5a; BLOCK_SIZE], metadata_target, 1, true)
        },
    )
    .expect("one full revoke descriptor and one metadata update must fit two credits");

    dev.commit().expect("commit revoke-credit transaction");
    let system = dev.system.as_ref().expect("journal state");
    assert_eq!(system.used_log_records, 4);
    assert_eq!(system.checkpoint_transactions.len(), 1);
}

#[test]
fn revoke_beyond_handle_request_fails_and_restores_the_transaction() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install csum-v3 journal");

    let error = dev
        .with_transaction_credits(TransactionCredits::metadata_with_revokes(0, 1), |dev| {
            dev.forget_detached_metadata(AbsoluteBN::new(10))?;
            dev.forget_detached_metadata(AbsoluteBN::new(11))
        })
        .expect_err("a handle must not consume an unrequested revoke record");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
    assert_eq!(
        error.context(),
        Some(crate::ErrorContext::Operation {
            op: "jbd2:revoke_credits"
        })
    );
    assert!(
        dev.system
            .as_ref()
            .unwrap()
            .running_transaction
            .revoked_blocks
            .is_empty(),
        "the failed handle must restore its revoke-table snapshot"
    );
}

#[test]
fn revoke_extension_charges_only_a_new_descriptor_boundary() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let revoke_records_per_block = dev
        .journal_revoke_records_per_block()
        .expect("revoke capacity");
    assert_eq!(dev.journal_transaction_capacity().unwrap(), 3);

    dev.with_transaction_credits(
        TransactionCredits::metadata_with_revokes(2, revoke_records_per_block - 1),
        |dev| {
            assert_eq!(
                dev.extend_transaction_credits(TransactionCredits::metadata_with_revokes(0, 1),)?,
                TransactionHandleExtension::Extended,
                "filling the existing revoke descriptor costs no new buffer credit"
            );
            assert_eq!(
                dev.extend_transaction_credits(TransactionCredits::metadata_with_revokes(0, 1),)?,
                TransactionHandleExtension::RestartRequired,
                "crossing the descriptor boundary exceeds the fixed transaction capacity"
            );
            let handle = dev.active_handle.as_ref().expect("active handle");
            assert_eq!(handle.revoke_credits_requested, revoke_records_per_block);
            assert_eq!(handle.revoke_credits_remaining, revoke_records_per_block);
            Ok(())
        },
    )
    .expect("the original revoke reservation remains valid");
}
