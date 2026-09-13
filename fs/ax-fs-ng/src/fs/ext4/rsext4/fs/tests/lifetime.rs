//! Allocation references exist independently of VFS wrapper publication.

use rsext4::{FileName, FilePermissions, MutationContext};

use super::*;

#[test]
fn unwrapped_inode_reference_prevents_reap_until_the_last_owner_is_dropped() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let (first, second) = {
        let mut state = filesystem.lock();
        let root = state.ext4.root_inode();
        let created = state
            .ext4
            .create_regular_file(
                MutationContext::new(0, 0, 0, 0),
                root,
                FileName::new(b"pending-publication").unwrap(),
                FilePermissions::new(0o600).unwrap(),
            )
            .unwrap();
        (
            state.retain_inode(&filesystem, created.number),
            state.retain_inode(&filesystem, created.number),
        )
    };
    let number = first.number();
    assert!(Arc::ptr_eq(first.content_access(), second.content_access()));
    {
        let mut state = filesystem.lock();
        let root = state.ext4.root_inode();
        let outcome = state
            .ext4
            .unlink(root, FileName::new(b"pending-publication").unwrap())
            .unwrap();
        assert!(outcome.requires_reap());
        assert_eq!(state.publish_zero_link(number), None);
        assert!(state.claim_pending_reap().is_none());
    }

    assert_eq!(first.metadata().unwrap().links, 0);
    drop(first);
    assert!(filesystem.lock().claim_pending_reap().is_none());
    assert_eq!(second.metadata().unwrap().links, 0);
    drop(second);

    let mut state = filesystem.lock();
    assert!(!state.has_pending_reaps());
    assert_eq!(
        state.ext4.inode(number).unwrap_err().kind(),
        rsext4::Ext4ErrorKind::NotFound
    );
}

#[test]
fn moving_an_allocation_reference_into_a_wrapper_does_not_register_it_twice() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let lifetime = {
        let mut state = filesystem.lock();
        let root = state.ext4.root_inode();
        state.retain_inode(&filesystem, root)
    };
    let number = lifetime.number();
    assert_eq!(filesystem.lock().lifetimes.live_refs.get(&number), Some(&1));
    let inode = Inode::new(lifetime, None);
    assert_eq!(filesystem.lock().lifetimes.live_refs.get(&number), Some(&1));
    drop(inode);
    assert!(!filesystem.lock().lifetimes.live_refs.contains_key(&number));
}
