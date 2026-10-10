//! Controlled I/O suspension exercises the actual shared cache state machine.

use alloc::{sync::Arc, vec, vec::Vec};
use std::{
    sync::{
        Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

use super::address_space::{BlockAddressSpace, FolioGeometry};
use crate::{BlockError, BlockResult, block::FsBlockDevice};

#[test]
fn blocked_writeback_allows_other_folios_and_keeps_redirty() {
    let tree = Arc::new(BlockAddressSpace::with_capacity(
        FolioGeometry::new(512).unwrap(),
        4,
    ));
    let mut device = Device::new();
    tree.write_buffered(&mut device, 0, 1, &[0x11; 512])
        .unwrap();
    let (mut paused, entered, resume) = device.paused_write(false);
    let worker_tree = Arc::clone(&tree);
    let worker = thread::spawn(move || worker_tree.writeback_dirty(&mut paused, None));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        tree.allocated_frames(),
        2,
        "resident and snapshot both consume budget"
    );

    let (finished, progress) = mpsc::sync_channel(1);
    let change_tree = Arc::clone(&tree);
    let mut change_device = device.clone();
    let changer = thread::spawn(move || {
        change_tree
            .write_buffered(&mut change_device, 8, 1, &[0x22; 512])
            .unwrap();
        change_tree
            .write_buffered(&mut change_device, 0, 1, &[0x33; 512])
            .unwrap();
        let mut read = [0; 512];
        change_tree
            .read_buffered(&mut change_device, 0, 1, &mut read)
            .unwrap();
        assert_eq!(read, [0x33; 512]);
        finished.send(()).unwrap();
    });
    let progressed = progress.recv_timeout(Duration::from_secs(2));
    // Always release the device before asserting progress, including when
    // testing an implementation which wrongly holds broad exclusion.
    resume.send(()).unwrap();
    worker.join().unwrap().unwrap();
    changer.join().unwrap();
    assert!(
        progressed.is_ok(),
        "unrelated writes and redirty must finish during I/O"
    );
    assert!(
        tree.has_dirty(),
        "old writeback must not clear a later generation"
    );
    tree.writeback_dirty(&mut device, None).unwrap();
    assert_eq!(&device.storage.lock().unwrap()[..512], &[0x33; 512]);
    assert!(!tree.has_dirty());
    assert_eq!(tree.allocated_frames(), 2);
}

#[test]
fn full_budget_writes_in_place_without_allocating_an_extra_frame() {
    let tree = Arc::new(BlockAddressSpace::with_capacity(
        FolioGeometry::new(512).unwrap(),
        1,
    ));
    let mut device = Device::new();
    tree.write_buffered(&mut device, 0, 1, &[0x44; 512])
        .unwrap();
    let (mut paused, entered, resume) = device.paused_write(false);
    let worker_tree = Arc::clone(&tree);
    let worker = thread::spawn(move || worker_tree.writeback_dirty(&mut paused, None));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let used = tree.allocated_frames();
    resume.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert_eq!(used, 1);
    assert!(!tree.has_dirty());
}

#[test]
fn failed_snapshot_preserves_the_latest_dirty_image() {
    let tree = Arc::new(BlockAddressSpace::with_capacity(
        FolioGeometry::new(512).unwrap(),
        4,
    ));
    let mut device = Device::new();
    tree.write_buffered(&mut device, 0, 1, &[0x55; 512])
        .unwrap();
    let (mut paused, entered, resume) = device.paused_write(true);
    let worker_tree = Arc::clone(&tree);
    let worker = thread::spawn(move || worker_tree.writeback_dirty(&mut paused, None));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let (finished, progress) = mpsc::sync_channel(1);
    let change_tree = Arc::clone(&tree);
    let mut change_device = device.clone();
    let changer = thread::spawn(move || {
        change_tree
            .write_buffered(&mut change_device, 0, 1, &[0x66; 512])
            .unwrap();
        finished.send(()).unwrap();
    });
    let progressed = progress.recv_timeout(Duration::from_secs(2));
    resume.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), Err(BlockError::Io));
    changer.join().unwrap();
    assert!(progressed.is_ok());
    assert!(tree.has_dirty());
    assert_eq!(tree.allocated_frames(), 1);
    tree.writeback_dirty(&mut device, None).unwrap();
    assert_eq!(&device.storage.lock().unwrap()[..512], &[0x66; 512]);
}

#[derive(Clone)]
struct Device {
    storage: Arc<Mutex<Vec<u8>>>,
    pause: Option<Arc<Pause>>,
}

struct Pause {
    entered: SyncSender<()>,
    resume: Mutex<Receiver<()>>,
    fail: bool,
}

impl Device {
    fn new() -> Self {
        Self {
            storage: Arc::new(Mutex::new(vec![0; 64 * 512])),
            pause: None,
        }
    }

    fn paused_write(&self, fail: bool) -> (Self, Receiver<()>, SyncSender<()>) {
        let (entered, observation) = mpsc::sync_channel(1);
        let (resume, wait) = mpsc::sync_channel(1);
        (
            Self {
                storage: Arc::clone(&self.storage),
                pause: Some(Arc::new(Pause {
                    entered,
                    resume: Mutex::new(wait),
                    fail,
                })),
            },
            observation,
            resume,
        )
    }
}

impl FsBlockDevice for Device {
    fn name(&self) -> &str {
        "cache-concurrency"
    }
    fn num_blocks(&self) -> u64 {
        64
    }
    fn block_size(&self) -> usize {
        512
    }

    fn read_block(&mut self, block: u64, out: &mut [u8]) -> BlockResult<()> {
        let start = block as usize * 512;
        out.copy_from_slice(&self.storage.lock().unwrap()[start..start + out.len()]);
        Ok(())
    }

    fn write_block(&mut self, block: u64, src: &[u8]) -> BlockResult<()> {
        if let Some(pause) = self.pause.take() {
            pause.entered.send(()).unwrap();
            pause.resume.lock().unwrap().recv().unwrap();
            if pause.fail {
                return Err(BlockError::Io);
            }
        }
        let start = block as usize * 512;
        self.storage.lock().unwrap()[start..start + src.len()].copy_from_slice(src);
        Ok(())
    }

    fn flush(&mut self) -> BlockResult<()> {
        Ok(())
    }
}
