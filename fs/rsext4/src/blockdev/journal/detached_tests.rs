//! Deterministic interleavings through the production journal owner.

use alloc::{sync::Arc, vec};
use std::sync::Mutex;

use super::*;
use crate::{
    Ext4ErrorKind,
    io::{DeviceCapabilities, DeviceGeometry, SectorId},
};

const BLOCK_BYTES: usize = 4096;
const HOME: AbsoluteBN = AbsoluteBN::new(10);

#[test]
fn sealed_commit_does_not_consume_later_running_updates() {
    let mut journal = journal();
    write_image(&mut journal, 0x11);
    let batch = journal.prepare_commit().unwrap();
    let ticket = batch.ticket();
    assert!(!journal.ticket_is_durable(&ticket).unwrap());

    write_image(&mut journal, 0x22);
    let mut receipt = batch.execute();
    journal.finish_commit(&mut receipt).unwrap();
    assert!(journal.ticket_is_durable(&ticket).unwrap());
    assert!(journal.has_running_updates());
    journal.read_block(HOME).unwrap();
    assert!(journal.buffer().iter().all(|byte| *byte == 0x22));
    let system = journal.system.as_ref().unwrap();
    assert!(
        system.checkpoint_transactions[0].updates[0]
            .1
            .iter()
            .all(|byte| *byte == 0x11)
    );
}

#[test]
fn foreign_receipt_remains_publishable_to_its_origin() {
    let mut origin = journal();
    let mut other = journal();
    write_image(&mut origin, 0x33);
    let batch = origin.prepare_commit().unwrap();
    let ticket = batch.ticket();
    let mut receipt = batch.execute();
    assert_eq!(
        other.finish_commit(&mut receipt).unwrap_err().kind(),
        Ext4ErrorKind::InvalidInput
    );
    origin.finish_commit(&mut receipt).unwrap();
    assert!(origin.ticket_is_durable(&ticket).unwrap());
    assert_eq!(
        origin.finish_commit(&mut receipt).unwrap_err().kind(),
        Ext4ErrorKind::InvalidInput
    );
}

#[test]
fn failed_commit_retains_sealed_images_and_original_error() {
    let mut journal = journal();
    write_image(&mut journal, 0x44);
    let batch = journal.prepare_commit().unwrap();
    let ticket = batch.ticket();
    journal.inner._device().storage.lock().unwrap().fail_flush = true;
    let mut receipt = batch.execute();
    let error = journal.finish_commit(&mut receipt).unwrap_err();
    assert_eq!(error, injected_flush_error());
    assert_eq!(journal.ticket_is_durable(&ticket).unwrap_err(), error);
    let sealed = journal
        .system
        .as_ref()
        .unwrap()
        .committing_transaction
        .as_ref()
        .unwrap();
    assert!(sealed.updates[0].1.iter().all(|byte| *byte == 0x44));
    assert!(journal.prepare_commit().is_err());
}

#[test]
fn later_background_sync_reports_the_original_io_failure_without_new_io() {
    let mut journal = journal();
    journal.enable_background_commits().unwrap();
    write_image(&mut journal, 0x45);
    let batch = journal.prepare_commit().unwrap();
    journal.inner._device().storage.lock().unwrap().fail_flush = true;
    let mut receipt = batch.execute();
    assert_eq!(
        journal.finish_commit(&mut receipt),
        Err(injected_flush_error())
    );
    let writes = journal.inner._device().storage.lock().unwrap().writes;
    assert_eq!(journal.prepare_abort().unwrap_err(), injected_flush_error());
    assert_eq!(
        journal.inner._device().storage.lock().unwrap().writes,
        writes
    );
}

#[test]
fn checkpoint_admission_rejects_changes_before_touching_the_device() {
    let mut journal = journal();
    write_image(&mut journal, 0x55);
    let batch = journal.prepare_commit().unwrap();
    journal.begin_checkpoint().unwrap();
    let mut receipt = batch.execute();
    journal.finish_commit(&mut receipt).unwrap();
    let checkpoint = journal.prepare_checkpoint().unwrap();
    let before = journal.inner._device().storage.lock().unwrap().writes;
    for metadata in [false, true] {
        assert_eq!(
            journal
                .write_blocks(&vec![0x66; BLOCK_BYTES], HOME, 1, metadata)
                .unwrap_err()
                .kind(),
            Ext4ErrorKind::Busy
        );
    }
    assert_eq!(
        journal.inner._device().storage.lock().unwrap().writes,
        before
    );
    let mut receipt = checkpoint.execute();
    journal.finish_commit(&mut receipt).unwrap();
    assert!(!journal.mutations_paused());
    write_image(&mut journal, 0x77);
}

#[test]
fn checkpoint_seal_closes_admission_only_after_preparation_succeeds() {
    let mut journal = journal();
    write_image(&mut journal, 0x53);
    journal.inner._device().storage.lock().unwrap().fail_fork = true;
    assert_eq!(
        journal.prepare_commit_for_checkpoint().unwrap_err(),
        injected_fork_error()
    );
    assert!(!journal.mutations_paused());
    assert!(journal.has_running_updates());
    journal.commits.ensure_idle().unwrap();

    journal.inner._device().storage.lock().unwrap().fail_fork = false;
    let batch = journal.prepare_commit_for_checkpoint().unwrap();
    assert!(journal.mutations_paused());
    assert!(!journal.has_running_updates());
    let mut receipt = batch.execute();
    journal.finish_commit(&mut receipt).unwrap();
    let mut receipt = journal.prepare_checkpoint().unwrap().execute();
    journal.finish_commit(&mut receipt).unwrap();
    assert!(!journal.mutations_paused());
}

#[test]
fn failed_checkpoint_preparation_keeps_a_retryable_owner() {
    let mut journal = journal();
    write_image(&mut journal, 0x54);
    let mut receipt = journal.prepare_commit_for_checkpoint().unwrap().execute();
    journal.finish_commit(&mut receipt).unwrap();
    journal.inner._device().storage.lock().unwrap().fail_fork = true;
    assert_eq!(
        journal.prepare_checkpoint().unwrap_err(),
        injected_fork_error()
    );
    assert!(journal.mutations_paused());
    assert_eq!(
        journal
            .system
            .as_ref()
            .unwrap()
            .checkpoint_transactions
            .len(),
        1
    );
    journal.commits.ensure_idle().unwrap();
    journal.inner._device().storage.lock().unwrap().fail_fork = false;
    let mut receipt = journal.prepare_checkpoint().unwrap().execute();
    journal.finish_commit(&mut receipt).unwrap();
    assert!(!journal.mutations_paused());
    write_image(&mut journal, 0x55);
}

#[test]
fn checkpoint_revoke_prevents_old_metadata_from_overwriting_reused_data() {
    let mut journal = journal();
    write_image(&mut journal, 0x88);
    let mut receipt = journal.prepare_commit().unwrap().execute();
    journal.finish_commit(&mut receipt).unwrap();
    journal
        .with_transaction_credits(TransactionCredits::metadata_with_revokes(1, 1), |journal| {
            journal.forget_detached_metadata(HOME)
        })
        .unwrap();
    journal
        .write_blocks(&vec![0x99; BLOCK_BYTES], HOME, 1, false)
        .unwrap();
    let batch = journal.prepare_commit().unwrap();
    journal.begin_checkpoint().unwrap();
    let mut receipt = batch.execute();
    journal.finish_commit(&mut receipt).unwrap();
    let mut receipt = journal.prepare_checkpoint().unwrap().execute();
    journal.finish_commit(&mut receipt).unwrap();
    let mut image = vec![0; BLOCK_BYTES];
    journal.read_blocks_uncached(&mut image, HOME, 1).unwrap();
    assert!(image.iter().all(|byte| *byte == 0x99));
}

#[test]
fn data_only_sync_needs_no_new_journal_records() {
    let mut journal = journal();
    loop {
        write_image(&mut journal, 0xaa);
        let mut receipt = journal.prepare_commit().unwrap().execute();
        journal.finish_commit(&mut receipt).unwrap();
        if journal.journal_available_log_records().unwrap()
            < journal.journal_maximum_transaction_records().unwrap()
        {
            break;
        }
    }
    let batch = journal
        .prepare_commit()
        .expect("a device flush requires no journal capacity");
    let ticket = batch.ticket();
    let mut receipt = batch.execute();
    journal.finish_commit(&mut receipt).unwrap();
    assert!(journal.ticket_is_durable(&ticket).unwrap());
}

#[test]
fn background_restart_can_attach_to_a_fresh_running_transaction() {
    let mut journal = journal();
    journal.enable_background_commits().unwrap();
    journal
        .restart_transaction(TransactionCredits::metadata(1), |journal| {
            journal.write_blocks(&vec![0xab; BLOCK_BYTES], HOME, 1, true)
        })
        .expect("an empty running transaction needs no external commit");
    let error = journal
        .restart_transaction(TransactionCredits::metadata(1), |_| Ok(()))
        .unwrap_err();
    assert!(error.requires_journal_progress());
    let batch = journal.prepare_commit().unwrap();
    journal
        .restart_transaction(TransactionCredits::metadata(1), |journal| {
            journal.write_blocks(&vec![0xcd; BLOCK_BYTES], HOME, 1, true)
        })
        .expect("the new running owner can proceed while the old commit is in flight");
    let mut receipt = batch.execute();
    journal.finish_commit(&mut receipt).unwrap();
    assert!(journal.has_running_updates());
}

#[test]
fn data_only_detached_owner_prevents_journal_reconfiguration() {
    let mut journal = journal();
    let batch = journal.prepare_commit().unwrap();
    assert_eq!(
        journal.set_journal_use(false).unwrap_err().kind(),
        Ext4ErrorKind::Busy
    );
    assert_eq!(
        journal
            .ensure_journal_state_reinstallable()
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::Busy
    );
    let mut receipt = batch.execute();
    journal.finish_commit(&mut receipt).unwrap();
    journal.set_journal_use(false).unwrap();
}

#[test]
fn abort_during_detached_io_is_persisted_only_by_the_receipt_owner() {
    for metadata in [false, true] {
        for before_io in [false, true] {
            let mut journal = journal();
            journal.enable_background_commits().unwrap();
            if metadata {
                write_image(&mut journal, 0x5a);
            }
            let batch = journal.prepare_commit().unwrap();
            let ticket = batch.ticket();
            let cause = Ext4Error::corrupted().with_operation("test:running_abort");
            if before_io {
                let writes = journal.inner._device().storage.lock().unwrap().writes;
                journal.abort_journal(cause);
                assert_eq!(
                    journal.inner._device().storage.lock().unwrap().writes,
                    writes
                );
            }
            let mut receipt = batch.execute();
            if !before_io {
                let writes = journal.inner._device().storage.lock().unwrap().writes;
                journal.abort_journal(cause);
                assert_eq!(
                    journal.inner._device().storage.lock().unwrap().writes,
                    writes
                );
            }
            assert_eq!(journal.finish_commit(&mut receipt), Err(cause));
            assert!(receipt.needs_abort_persistence());
            receipt.persist_abort();
            assert!(!receipt.needs_abort_persistence());
            assert_eq!(journal.finish_commit(&mut receipt), Err(cause));
            assert_eq!(journal.ticket_is_durable(&ticket), Err(cause));
            let storage = journal.inner._device().storage.lock().unwrap();
            let offset = 128 * BLOCK_BYTES + 32;
            assert_ne!(
                u32::from_be_bytes(storage.bytes[offset..offset + 4].try_into().unwrap()),
                0
            );
        }
    }
}

#[test]
fn idle_background_abort_defers_device_io_to_an_owned_batch() {
    let mut journal = journal();
    journal.enable_background_commits().unwrap();
    write_image(&mut journal, 0x21);
    let writes = journal.inner._device().storage.lock().unwrap().writes;
    let cause = Ext4Error::corrupted().with_operation("test:idle_abort");
    journal.abort_journal(cause);
    assert_eq!(
        journal.inner._device().storage.lock().unwrap().writes,
        writes
    );
    let batch = journal
        .prepare_abort()
        .unwrap()
        .expect("aborted mount needs an I/O owner");
    assert_eq!(
        journal.inner._device().storage.lock().unwrap().writes,
        writes
    );
    let mut receipt = batch.execute();
    assert_eq!(journal.finish_commit(&mut receipt), Err(cause));
    assert!(!receipt.needs_abort_persistence());
    assert_eq!(journal.prepare_abort().unwrap_err(), cause);
    assert!(
        journal.has_running_updates(),
        "failed dirty metadata stays owned"
    );
}

fn write_image(journal: &mut Jbd2Dev<SharedDevice>, value: u8) {
    journal
        .write_blocks(&vec![value; BLOCK_BYTES], HOME, 1, true)
        .unwrap();
}

pub(super) fn journal() -> Jbd2Dev<SharedDevice> {
    let device = SharedDevice {
        storage: Arc::new(Mutex::new(Storage {
            bytes: vec![0; 256 * BLOCK_BYTES],
            writes: 0,
            fail_flush: false,
            fail_fork: false,
        })),
    };
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    journal
        .set_journal_superblock(
            JournalSuperBlock {
                s_maxlen: 64,
                s_first: 1,
                s_feature_incompat: JBD2_FEATURE_INCOMPAT_REVOKE,
                ..JournalSuperBlock::default()
            },
            AbsoluteBN::new(128),
        )
        .unwrap();
    journal
}

fn injected_flush_error() -> Ext4Error {
    Ext4Error::io().with_operation("test:detached_flush_failure")
}

fn injected_fork_error() -> Ext4Error {
    Ext4Error::no_memory().with_operation("test:detached_fork_failure")
}

struct Storage {
    bytes: Vec<u8>,
    writes: usize,
    fail_flush: bool,
    fail_fork: bool,
}

#[derive(Clone)]
pub(super) struct SharedDevice {
    storage: Arc<Mutex<Storage>>,
}

impl Clock for SharedDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(1, 0))
    }
}

impl ForkBlockIo for SharedDevice {
    fn fork_io(&self) -> Ext4Result<Self> {
        if self.storage.lock().unwrap().fail_fork {
            Err(injected_fork_error())
        } else {
            Ok(self.clone())
        }
    }
}

impl BlockIo for SharedDevice {
    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(512, (256 * BLOCK_BYTES / 512) as u64)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            ..DeviceCapabilities::default()
        }
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        assert_eq!(buffer.len(), count as usize * 512);
        let start = sector.as_usize()? * 512;
        let mut storage = self.storage.lock().unwrap();
        storage.bytes[start..start + buffer.len()].copy_from_slice(buffer);
        storage.writes += 1;
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        assert_eq!(buffer.len(), count as usize * 512);
        let start = sector.as_usize()? * 512;
        buffer.copy_from_slice(&self.storage.lock().unwrap().bytes[start..start + buffer.len()]);
        Ok(())
    }

    fn flush(&mut self) -> Ext4Result<()> {
        if self.storage.lock().unwrap().fail_flush {
            Err(injected_flush_error())
        } else {
            Ok(())
        }
    }
}
