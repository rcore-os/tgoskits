//! Journal abort contracts.

use super::*;

#[test]
fn commit_fault_matrix_aborts_at_every_write_and_flush_boundary() {
    let write_stages = [
        "open-superblock",
        "descriptor",
        "payload",
        "commit",
        "checkpoint",
        "close-superblock",
    ];
    for (index, stage) in write_stages.iter().enumerate() {
        assert_commit_stage_fault_aborts_journal(
            MemBlockDev::with_failing_write_call(256, index + 1),
            stage,
        );
    }

    let flush_stages = ["descriptor-payload-barrier", "checkpoint-barrier"];
    for (index, stage) in flush_stages.iter().enumerate() {
        assert_commit_stage_fault_aborts_journal(
            MemBlockDev::with_failing_flush_call(256, index + 1),
            stage,
        );
    }
}

#[test]
fn commit_failure_aborts_future_journal_operations() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");

    let first_error = dev
        .umount_commit()
        .expect_err("first failed commit must propagate the device error");
    assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);
    let system = dev.system.as_ref().expect("journal state");
    assert!(
        system.running_transaction.updates.is_empty(),
        "a transaction that entered commit I/O must no longer be owned by the running queue"
    );
    let committing = system
        .committing_transaction
        .as_ref()
        .expect("failed transaction remains owned by the committing state");
    assert_eq!(committing.updates.len(), 1);
    assert_eq!(committing.phase, Jbd2CommitPhase::DataFlush);

    let write_error = dev
        .write_block(AbsoluteBN::new(11), true)
        .expect_err("an aborted journal must reject later metadata writes");
    assert_eq!(write_error.kind(), crate::Ext4ErrorKind::JournalAborted);

    let handle_error = dev
        .with_journal_handle(1, |_| Ok(()))
        .expect_err("an aborted journal must reject later handles");
    assert_eq!(handle_error.kind(), crate::Ext4ErrorKind::JournalAborted);

    let flush_error = dev
        .flush()
        .expect_err("an aborted journal must reject later flushes");
    assert_eq!(flush_error.kind(), crate::Ext4ErrorKind::JournalAborted);

    let unmount_error = dev
        .umount_commit()
        .expect_err("an aborted journal must not retry the transaction");
    assert_eq!(unmount_error.kind(), crate::Ext4ErrorKind::JournalAborted);

    let mode_error = dev
        .set_journal_use(false)
        .expect_err("an abort must reject journal mode changes");
    assert_eq!(mode_error.kind(), crate::Ext4ErrorKind::JournalAborted);
    let bypass_error = dev
        .write_block(AbsoluteBN::new(13), false)
        .expect_err("disabling journal use must not clear an abort");
    assert_eq!(bypass_error.kind(), crate::Ext4ErrorKind::JournalAborted);

    let reinstall_error = dev
        .set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect_err("reinstalling state must not clear an abort on the same mount object");
    assert_eq!(reinstall_error.kind(), crate::Ext4ErrorKind::JournalAborted);
}

#[test]
fn commit_failure_persists_recorded_error_with_fua() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
    let superblock = csum_v3_superblock();
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .expect("install checksummed journal");
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");

    let first_error = dev
        .umount_commit()
        .expect_err("first failed commit must propagate the device error");
    assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);

    let inner = dev.into_inner();
    assert_eq!(inner.fua_writes, 1, "abort errno must use one FUA write");
    let journal_offset = 128 * BLOCK_SIZE;
    let recorded =
        JournalSuperBlock::from_disk_bytes(&inner.data[journal_offset..journal_offset + 1024]);
    assert_eq!(
        recorded.s_errno, 0xffff_fffb,
        "JBD2 stores the private generic I/O abort wire code"
    );
    assert_eq!(
        &inner.data[journal_offset + 32..journal_offset + 36],
        &[0xff, 0xff, 0xff, 0xfb]
    );
    assert_eq!(recorded.s_sequence, superblock.s_sequence);
    assert_eq!(recorded.s_start, superblock.s_first);

    let mut remount = Jbd2Dev::initial_jbd2dev(0, inner, true);
    remount
        .set_journal_superblock(recorded, AbsoluteBN::new(128))
        .expect("a later mount must load a journal error from the previous lifetime");
    assert!(remount.has_recorded_journal_error());

    remount.inner._device_mut().fail_fua = true;
    let clear_error = remount
        .clear_recorded_journal_error()
        .expect_err("a failed FUA must not clear the in-memory journal error");
    assert_eq!(clear_error.kind(), crate::Ext4ErrorKind::Io);
    assert!(remount.has_recorded_journal_error());

    remount.inner._device_mut().fail_fua = false;
    remount
        .clear_recorded_journal_error()
        .expect("the recorded error must clear durably after ext4 records it");
    assert!(!remount.has_recorded_journal_error());
    let inner = remount.into_inner();
    assert_eq!(inner.fua_writes, 3);
    let cleared =
        JournalSuperBlock::from_disk_bytes(&inner.data[journal_offset..journal_offset + 1024]);
    assert_eq!(cleared.s_errno, 0);
}

#[test]
fn automatic_commit_failure_aborts_the_journal() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    let capacity = dev.journal_transaction_capacity().unwrap();
    let updates = vec![0x5a; capacity * BLOCK_SIZE];
    dev.write_blocks(
        &updates,
        AbsoluteBN::new(10),
        u32::try_from(capacity).unwrap(),
        true,
    )
    .expect("fill one transaction");

    let first_error = dev
        .write_blocks(
            &vec![0x5a; BLOCK_SIZE],
            AbsoluteBN::new(10 + capacity as u64),
            1,
            true,
        )
        .expect_err("queue overflow must propagate the automatic commit failure");
    assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);

    let unmount_error = dev
        .umount_commit()
        .expect_err("an aborted automatic commit must not be retried");
    assert_eq!(unmount_error.kind(), crate::Ext4ErrorKind::JournalAborted);
}

#[test]
fn journal_cannot_be_disabled_with_pending_updates() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
        .expect("install small journal");
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");

    let error = dev
        .set_journal_use(false)
        .expect_err("pending journal state must not be bypassed");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
    assert!(dev.is_use_journal());

    dev.umount_commit().expect("commit pending update");
    dev.set_journal_use(false)
        .expect("disable journal after commit");
    assert!(!dev.is_use_journal());
}

#[test]
fn replay_without_journal_state_latches_abort() {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);

    let failure = dev
        .journal_replay_checked()
        .failure()
        .expect("replay cannot proceed without installed journal state");
    assert_eq!(failure.phase(), JournalReplayPhase::Initialize);
    assert_eq!(failure.cause().kind(), crate::Ext4ErrorKind::JournalAborted);
    let error = dev
        .set_journal_use(false)
        .expect_err("an incomplete replay must latch the journal abort");
    assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
}

fn assert_commit_stage_fault_aborts_journal(device: MemBlockDev, stage: &str) {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
        .expect("install checksummed journal");
    dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
        .expect("queue metadata update");

    let first_error = match dev.umount_commit() {
        Ok(()) => panic!("{stage} fault must fail commit"),
        Err(error) => error,
    };
    assert_eq!(
        first_error.kind(),
        crate::Ext4ErrorKind::Io,
        "{stage} must preserve the device I/O error"
    );
    let state = dev
        .abort_state
        .as_ref()
        .expect("stage fault must abort journal");
    assert_eq!(state.cause.kind(), crate::Ext4ErrorKind::Io, "{stage}");
    assert_eq!(state.persistence_error, None, "{stage}");
    let expected_fua_writes = match stage {
        "open-superblock" | "descriptor" | "payload" | "descriptor-payload-barrier" => 1,
        "commit" | "checkpoint" | "checkpoint-barrier" => 2,
        "close-superblock" => 3,
        _ => panic!("unknown commit fault stage: {stage}"),
    };
    assert_eq!(
        dev.inner._device().fua_writes,
        expected_fua_writes,
        "{stage}"
    );
    assert_eq!(dev.inner._device().fail_write_call, None, "{stage}");
    assert_eq!(dev.inner._device().fail_flush_call, None, "{stage}");

    let later_error = dev
        .write_block(AbsoluteBN::new(11), true)
        .expect_err("stage fault must make abort sticky");
    assert_eq!(
        later_error.kind(),
        crate::Ext4ErrorKind::JournalAborted,
        "{stage}"
    );
}
