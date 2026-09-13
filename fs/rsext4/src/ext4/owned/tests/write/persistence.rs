//! Durable snapshots distinguish allocated, written and published extents.

use super::*;

#[test]
fn committed_unwritten_mapping_never_exposes_unpublished_data_after_remount() {
    let (mut mount, number) = shared_file();
    let input = alloc::vec![0x49; 8192 + 7];
    let prepared = mount
        .prepare_inode_write(number, 0, &input)
        .unwrap()
        .unwrap();
    mount.sync().unwrap();
    assert_hidden_after_remount(&mount, number, input.len());

    let mut completed = prepared.execute();
    // Commit/checkpoint may run after ordinary data completion but before its
    // conversion receipt. The persisted mapping must still read as unwritten.
    mount.sync().unwrap();
    assert_hidden_after_remount(&mount, number, input.len());

    mount.finish_inode_write(&mut completed).unwrap();
    mount.sync().unwrap();
    let mut recovered = remount_snapshot(&mount);
    assert_contents(&mut recovered, number, &input);
    recovered.unmount().unwrap();
}

#[test]
fn a_later_failed_data_run_does_not_initialize_an_earlier_successful_run() {
    let (mut mount, number) = shared_file();
    let input = alloc::vec![0x6c; 1024 * 1024 + 4096];
    let prepared = mount
        .prepare_inode_write(number, 0, &input)
        .unwrap()
        .unwrap();
    let cause = Ext4Error::io().with_operation("test:second_data_run");
    let probe = watch(Some(cause), None);
    DATA_IO.with_borrow_mut(|probe| {
        probe.as_mut().unwrap().successful_writes_before_error = 1;
    });
    let mut completed = prepared.execute();
    DATA_IO.with_borrow(|probe| assert_eq!(probe.as_ref().unwrap().writes.len(), 2));
    drop(probe);
    assert_eq!(mount.finish_inode_write(&mut completed), Err(cause));
    assert!(!completed.needs_publication());
    mount.sync().unwrap();
    assert_hidden_after_remount(&mount, number, input.len());
    mount.truncate_inode(number, input.len() as u64).unwrap();
    assert_contents(&mut mount, number, &alloc::vec![0; input.len()]);
}

fn assert_hidden_after_remount(mount: &TestMount, number: InodeNumber, length: usize) {
    let mut recovered = remount_snapshot(mount);
    assert_eq!(recovered.inode(number).unwrap().size, 0);
    recovered.truncate_inode(number, length as u64).unwrap();
    assert_contents(&mut recovered, number, &alloc::vec![0; length]);
    recovered.unmount().unwrap();
}

fn remount_snapshot(mount: &TestMount) -> TestMount {
    let endpoint = mount.device.fork_read_endpoint().unwrap();
    // Copy the completed sync boundary, not a shared live device. This tests
    // actual on-disk extent state and replay, not volatile drive-cache loss.
    let device = MemoryDevice {
        bytes: Arc::new(Mutex::new(endpoint.bytes.lock().unwrap().clone())),
    };
    Ext4::mount(
        device,
        MountServices::new(TestClock, (), NoopObserver),
        MountOptions::read_write(),
    )
    .unwrap()
}
