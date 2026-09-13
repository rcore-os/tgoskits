//! Journal handles contracts.

use super::*;

#[test]
fn journal_handle_credit_overrun_restores_queued_updates() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let target = AbsoluteBN::new(10);
    let updates = vec![0x5a; BLOCK_SIZE * 2];

    let error = dev
        .with_journal_handle(1, |dev| dev.write_blocks(&updates, target, 2, true))
        .expect_err("one credit cannot journal two distinct metadata blocks");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);

    dev.umount_commit().expect("aborted handle left no updates");
    let inner = dev.into_inner();
    for block in [target, target.checked_add(1).unwrap()] {
        let start = block.as_usize().unwrap() * BLOCK_SIZE;
        assert!(
            inner.data[start..start + BLOCK_SIZE]
                .iter()
                .all(|&byte| byte == 0)
        );
    }
}

#[test]
fn failed_journal_handle_does_not_write_dirty_metadata_cache_home() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let target = AbsoluteBN::new(10);
    dev.read_block(target).expect("cache clean home block");

    let error = dev
        .with_journal_handle(1, |dev| {
            dev.buffer_mut()[0] = 0x5a;
            dev.write_block(target, true)?;
            Err::<(), _>(Ext4Error::io())
        })
        .expect_err("operation failure must abort the handle update");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

    let inner = dev.into_inner();
    let start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        inner.data[start], 0,
        "aborted journal metadata reached its home block"
    );
}

#[test]
fn failed_journal_handle_restores_replaced_pending_update() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let target = AbsoluteBN::new(10);
    dev.write_blocks(&vec![0x11; BLOCK_SIZE], target, 1, true)
        .expect("queue previous transaction update");

    let error = dev
        .with_journal_handle(1, |dev| {
            dev.write_blocks(&vec![0x22; BLOCK_SIZE], target, 1, true)?;
            Err::<(), _>(Ext4Error::io())
        })
        .expect_err("operation failure must abort the handle updates");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

    let mut observed = vec![0; BLOCK_SIZE];
    dev.read_blocks(&mut observed, target, 1)
        .expect("read restored pending update");
    assert_eq!(observed, vec![0x11; BLOCK_SIZE]);
    dev.umount_commit().expect("commit restored pending update");
    let inner = dev.into_inner();
    let start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[start..start + BLOCK_SIZE],
        &vec![0x11; BLOCK_SIZE]
    );
}

#[test]
fn journal_handle_reserves_space_before_operation_without_auto_split() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let capacity = dev.journal_transaction_capacity().unwrap();
    let older_target = AbsoluteBN::new(10);
    let older_updates = vec![0x11; (capacity - 1) * BLOCK_SIZE];
    dev.write_blocks(
        &older_updates,
        older_target,
        u32::try_from(capacity - 1).unwrap(),
        true,
    )
    .expect("queue older running-transaction updates");
    let sequence_before = dev.journal_sequence().unwrap();

    let new_target = AbsoluteBN::new(32);
    let new_updates = vec![0x22; 2 * BLOCK_SIZE];
    let sequence_inside = dev
        .with_journal_handle(2, |dev| {
            let sequence_after_reservation = dev.journal_sequence().unwrap();
            dev.write_blocks(&new_updates, new_target, 2, true)?;
            assert_eq!(dev.journal_sequence(), Some(sequence_after_reservation));
            Ok(sequence_after_reservation)
        })
        .expect("reserved handle must keep one operation in the running transaction");

    assert_eq!(sequence_inside, sequence_before.wrapping_add(1));
    assert_eq!(dev.journal_sequence(), Some(sequence_inside));
    dev.umount_commit().expect("commit handle updates");
    assert_eq!(
        dev.journal_sequence(),
        Some(sequence_inside.wrapping_add(1))
    );
}

#[test]
fn new_handle_checkpoints_before_dirtying_when_log_lacks_maximum_transaction_space() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let capacity = dev.journal_transaction_capacity().unwrap();
    let maximum_records = dev.journal_maximum_transaction_records().unwrap();
    assert_eq!((capacity, maximum_records), (3, 5));

    for transaction in 0..3u64 {
        let first_target = AbsoluteBN::new(10 + transaction * capacity as u64);
        dev.with_journal_handle(capacity, |dev| {
            for offset in 0..capacity {
                dev.write_blocks(
                    &vec![transaction as u8 + 1; BLOCK_SIZE],
                    first_target.checked_add(u32::try_from(offset).unwrap())?,
                    1,
                    true,
                )?;
            }
            Ok(())
        })
        .expect("fill one maximum-sized transaction");
        dev.commit().expect("commit maximum-sized transaction");
    }
    assert_eq!(dev.journal_available_log_records().unwrap(), 0);
    assert_eq!(
        dev.system.as_ref().unwrap().checkpoint_transactions.len(),
        3
    );

    dev.with_journal_handle(1, |dev| {
        assert_eq!(dev.journal_available_log_records()?, maximum_records);
        assert_eq!(
            dev.system.as_ref().unwrap().checkpoint_transactions.len(),
            2,
            "space must be reclaimed before the operation can dirty metadata"
        );
        assert_eq!(
            dev.extend_transaction_credits(TransactionCredits::metadata(capacity - 1))?,
            TransactionHandleExtension::Extended,
            "extend uses the already guaranteed log reservation"
        );
        Ok(())
    })
    .expect("a new handle must reserve one maximum transaction of log space");
}

#[test]
fn unscoped_metadata_write_reserves_log_space_before_starting_a_transaction() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let capacity = dev.journal_transaction_capacity().unwrap();
    for transaction in 0..3u64 {
        let first_target = AbsoluteBN::new(10 + transaction * capacity as u64);
        dev.with_journal_handle(capacity, |dev| {
            for offset in 0..capacity {
                dev.write_blocks(
                    &vec![transaction as u8 + 1; BLOCK_SIZE],
                    first_target.checked_add(u32::try_from(offset).unwrap())?,
                    1,
                    true,
                )?;
            }
            Ok(())
        })
        .expect("fill one maximum-sized transaction");
        dev.commit().expect("commit maximum-sized transaction");
    }
    assert_eq!(dev.journal_available_log_records().unwrap(), 0);

    dev.write_blocks(&vec![0x7e; BLOCK_SIZE], AbsoluteBN::new(64), 1, true)
        .expect("unscoped write must reclaim space before starting a transaction");
    assert_eq!(dev.journal_available_log_records().unwrap(), 5);
    assert_eq!(
        dev.system.as_ref().unwrap().checkpoint_transactions.len(),
        2
    );
}

#[test]
fn invalid_bulk_buffer_does_not_precommit_older_updates() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let capacity = dev.journal_transaction_capacity().unwrap();
    dev.write_blocks(
        &vec![0x11; (capacity - 1) * BLOCK_SIZE],
        AbsoluteBN::new(10),
        u32::try_from(capacity - 1).unwrap(),
        true,
    )
    .expect("queue older updates");
    let sequence = dev.journal_sequence();

    let error = dev
        .write_blocks(&vec![0x22; BLOCK_SIZE], AbsoluteBN::new(32), 2, true)
        .expect_err("short input cannot satisfy a two-block write");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::InvalidInput);
    assert_eq!(dev.journal_sequence(), sequence);
}

#[test]
fn journal_handle_charges_one_credit_per_distinct_block() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let target = AbsoluteBN::new(10);

    dev.with_journal_handle(1, |dev| {
        dev.write_blocks(&vec![0x11; BLOCK_SIZE], target, 1, true)?;
        dev.write_blocks(&vec![0x22; BLOCK_SIZE], target, 1, true)
    })
    .expect("replacing one metadata block consumes one credit");

    let mut observed = vec![0; BLOCK_SIZE];
    dev.read_blocks(&mut observed, target, 1)
        .expect("read final queued update");
    assert_eq!(observed, vec![0x22; BLOCK_SIZE]);
    dev.umount_commit().expect("commit final queued update");
    let inner = dev.into_inner();
    let start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[start..start + BLOCK_SIZE],
        &vec![0x22; BLOCK_SIZE]
    );
}

#[test]
fn journal_handle_extends_before_touching_an_additional_block() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let first = AbsoluteBN::new(10);
    let second = AbsoluteBN::new(11);

    dev.with_journal_handle(1, |dev| {
        dev.write_blocks(&vec![0x31; BLOCK_SIZE], first, 1, true)?;
        assert_eq!(
            dev.extend_transaction_credits(TransactionCredits::metadata(1))?,
            TransactionHandleExtension::Extended
        );
        dev.write_blocks(&vec![0x42; BLOCK_SIZE], second, 1, true)
    })
    .expect("extended handle must reserve the second metadata block");

    dev.umount_commit().expect("commit extended handle update");
    let inner = dev.into_inner();
    let first_start = first.as_usize().unwrap() * BLOCK_SIZE;
    let second_start = second.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[first_start..first_start + BLOCK_SIZE],
        &vec![0x31; BLOCK_SIZE]
    );
    assert_eq!(
        &inner.data[second_start..second_start + BLOCK_SIZE],
        &vec![0x42; BLOCK_SIZE]
    );
}

#[test]
fn transaction_restart_switches_before_attaching_the_next_handle() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let first = AbsoluteBN::new(10);
    let second = AbsoluteBN::new(11);

    dev.with_transaction_handle(1, |dev| {
        dev.write_blocks(&vec![0x35; BLOCK_SIZE], first, 1, true)
    })
    .expect("publish the old transaction step");
    let old_sequence = dev.journal_sequence().expect("old transaction sequence");

    dev.restart_transaction(TransactionCredits::metadata(1), |dev| {
        assert_ne!(
            dev.journal_sequence(),
            Some(old_sequence),
            "restart must switch the old transaction before the new handle attaches"
        );
        dev.write_blocks(&vec![0x46; BLOCK_SIZE], second, 1, true)
    })
    .expect("restart into the next transaction");

    let system = dev.system.as_ref().expect("journal state");
    assert_eq!(system.checkpoint_transactions.len(), 1);
    assert_eq!(system.checkpoint_transactions[0].updates.len(), 1);
    assert_eq!(system.running_transaction.updates.len(), 1);
    dev.umount_commit().expect("commit both transaction steps");
    let inner = dev.into_inner();
    let first_start = first.as_usize().unwrap() * BLOCK_SIZE;
    let second_start = second.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[first_start..first_start + BLOCK_SIZE],
        &vec![0x35; BLOCK_SIZE]
    );
    assert_eq!(
        &inner.data[second_start..second_start + BLOCK_SIZE],
        &vec![0x46; BLOCK_SIZE]
    );
}

#[test]
fn transaction_restart_preserves_a_detached_reserved_handle() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let first = AbsoluteBN::new(10);
    let second = AbsoluteBN::new(11);
    let third = AbsoluteBN::new(12);
    let ((), reserved) = dev
        .with_transaction_reservation(
            TransactionCredits::metadata(1),
            TransactionCredits::metadata(1),
            |dev| dev.write_blocks(&vec![0x51; BLOCK_SIZE], first, 1, true),
        )
        .expect("publish parent and detach child reservation");
    let old_sequence = dev.journal_sequence().expect("old transaction sequence");

    dev.restart_transaction(TransactionCredits::metadata(1), |dev| {
        assert_ne!(dev.journal_sequence(), Some(old_sequence));
        dev.write_blocks(&vec![0x62; BLOCK_SIZE], second, 1, true)
    })
    .expect("restart while retaining detached child credits");
    let new_sequence = dev.journal_sequence().expect("new transaction sequence");
    dev.with_reserved_transaction(reserved, |dev| {
        assert_eq!(dev.journal_sequence(), Some(new_sequence));
        dev.write_blocks(&vec![0x73; BLOCK_SIZE], third, 1, true)
    })
    .expect("attach child to the restarted transaction");

    dev.umount_commit().expect("commit restarted transaction");
    let inner = dev.into_inner();
    for (block, byte) in [(first, 0x51), (second, 0x62), (third, 0x73)] {
        let start = block.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[start..start + BLOCK_SIZE],
            &vec![byte; BLOCK_SIZE]
        );
    }
}

#[test]
fn transaction_restart_rejects_an_active_handle_without_committing_it() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let sequence = dev.journal_sequence().expect("journal sequence");

    dev.with_transaction_handle(1, |dev| {
        let mut operation_started = false;
        let error = dev
            .restart_transaction(TransactionCredits::metadata(1), |_| {
                operation_started = true;
                Ok(())
            })
            .expect_err("restart must begin only after the old handle stops");
        assert!(!operation_started);
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        assert_eq!(dev.journal_sequence(), Some(sequence));
        Ok(())
    })
    .expect("outer handle remains valid");
}

#[test]
fn failed_journal_handle_extension_preserves_the_original_reservation() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let first = AbsoluteBN::new(10);
    let second = AbsoluteBN::new(11);

    dev.with_journal_handle(1, |dev| {
        dev.write_blocks(&vec![0x53; BLOCK_SIZE], first, 1, true)?;
        assert_eq!(
            dev.extend_transaction_credits(TransactionCredits::metadata(usize::MAX))?,
            TransactionHandleExtension::RestartRequired
        );
        let error = dev
            .write_blocks(&vec![0x64; BLOCK_SIZE], second, 1, true)
            .expect_err("failed extension must not change the original one-credit handle");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
        Ok(())
    })
    .expect("the original handle remains valid after best-effort extension fails");

    dev.umount_commit().expect("commit original handle update");
    let inner = dev.into_inner();
    let first_start = first.as_usize().unwrap() * BLOCK_SIZE;
    let second_start = second.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[first_start..first_start + BLOCK_SIZE],
        &vec![0x53; BLOCK_SIZE]
    );
    assert_eq!(
        &inner.data[second_start..second_start + BLOCK_SIZE],
        &vec![0; BLOCK_SIZE]
    );
}

#[test]
fn journal_handle_extension_accounts_for_the_existing_running_transaction() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    dev.write_blocks(&vec![0x17; BLOCK_SIZE], AbsoluteBN::new(9), 1, true)
        .expect("queue metadata before starting the handle");

    dev.with_journal_handle(1, |dev| {
        dev.write_blocks(&vec![0x28; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)?;
        assert_eq!(
            dev.extend_transaction_credits(TransactionCredits::metadata(1))?,
            TransactionHandleExtension::Extended
        );
        assert_eq!(
            dev.extend_transaction_credits(TransactionCredits::metadata(1))?,
            TransactionHandleExtension::RestartRequired,
            "the pre-handle update must remain part of the reservation"
        );
        Ok(())
    })
    .expect("the full transaction capacity must remain usable");
}

#[test]
fn direct_metadata_handle_extension_expands_the_rollback_owner() {
    let first = AbsoluteBN::new(10);
    let second = AbsoluteBN::new(11);
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), false);

    let error = dev
        .with_transaction_handle(1, |dev| {
            dev.write_blocks(&vec![0x39; BLOCK_SIZE], first, 1, true)?;
            assert_eq!(
                dev.extend_transaction_credits(TransactionCredits::metadata(1))?,
                TransactionHandleExtension::Extended
            );
            dev.write_blocks(&vec![0x4a; BLOCK_SIZE], second, 1, true)?;
            Err::<(), _>(Ext4Error::io().with_operation("test:direct_extended_abort"))
        })
        .expect_err("operation failure must restore every extended direct-write block");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

    let mut first_after = vec![0xff; BLOCK_SIZE];
    let mut second_after = vec![0xff; BLOCK_SIZE];
    dev.read_blocks(&mut first_after, first, 1)
        .expect("read restored first direct block");
    dev.read_blocks(&mut second_after, second, 1)
        .expect("read restored second direct block");
    assert_eq!(first_after, vec![0; BLOCK_SIZE]);
    assert_eq!(second_after, vec![0; BLOCK_SIZE]);
}

#[test]
fn nested_journal_handle_reuses_outer_credits_and_transaction() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let outer_target = AbsoluteBN::new(10);
    let nested_target = AbsoluteBN::new(11);
    let sequence = dev.journal_sequence();

    dev.with_journal_handle(2, |dev| {
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], outer_target, 1, true)?;
        dev.with_journal_handle(usize::MAX, |dev| {
            assert_eq!(dev.journal_sequence(), sequence);
            dev.write_blocks(&vec![0xa5; BLOCK_SIZE], nested_target, 1, true)
        })
    })
    .expect("nested start reuses the current owner and its credits");

    assert_eq!(dev.journal_sequence(), sequence);
    dev.umount_commit().expect("commit outer handle update");
    let inner = dev.into_inner();
    let start = outer_target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[start..start + BLOCK_SIZE],
        &vec![0x5a; BLOCK_SIZE]
    );
    let start = nested_target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[start..start + BLOCK_SIZE],
        &vec![0xa5; BLOCK_SIZE]
    );
}

#[test]
fn failed_nested_journal_handle_restores_only_its_scope() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let outer_target = AbsoluteBN::new(10);
    let failed_target = AbsoluteBN::new(11);

    dev.with_journal_handle(2, |dev| {
        dev.write_blocks(&vec![0x11; BLOCK_SIZE], outer_target, 1, true)?;
        let error = dev
            .with_journal_handle(1, |dev| {
                dev.write_blocks(&vec![0xa5; BLOCK_SIZE], failed_target, 1, true)?;
                Err::<(), _>(Ext4Error::io().with_operation("test:nested_operation_failure"))
            })
            .expect_err("nested operation must propagate its failure");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

        let mut observed = vec![0xff; BLOCK_SIZE];
        dev.read_blocks(&mut observed, failed_target, 1)?;
        assert_eq!(observed, vec![0; BLOCK_SIZE]);
        dev.write_blocks(&vec![0x22; BLOCK_SIZE], outer_target, 1, true)
    })
    .expect("outer handle remains usable after nested rollback");

    dev.umount_commit().expect("commit outer handle update");
    let inner = dev.into_inner();
    let outer_start = outer_target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[outer_start..outer_start + BLOCK_SIZE],
        &vec![0x22; BLOCK_SIZE]
    );
    let failed_start = failed_target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[failed_start..failed_start + BLOCK_SIZE],
        &vec![0; BLOCK_SIZE]
    );
}

#[test]
fn failed_nested_handle_restores_revoke_credits_and_table() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");

    dev.with_transaction_credits(TransactionCredits::metadata_with_revokes(0, 2), |dev| {
        let error = dev
            .with_journal_handle(usize::MAX, |dev| {
                dev.forget_detached_metadata(AbsoluteBN::new(10))?;
                Err::<(), _>(Ext4Error::io().with_operation("test:nested_revoke_abort"))
            })
            .expect_err("nested revoke failure must roll back its scope");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);
        assert!(
            dev.system
                .as_ref()
                .unwrap()
                .running_transaction
                .revoked_blocks
                .is_empty()
        );
        assert_eq!(
            dev.active_handle.as_ref().unwrap().revoke_credits_remaining,
            2
        );
        dev.forget_detached_metadata(AbsoluteBN::new(10))?;
        dev.forget_detached_metadata(AbsoluteBN::new(11))
    })
    .expect("the outer handle must retain both revoke records");

    dev.commit().expect("commit restored outer revoke scope");
    assert_eq!(
        dev.system.as_ref().unwrap().checkpoint_transactions[0]
            .revoked_blocks
            .len(),
        2
    );
}
