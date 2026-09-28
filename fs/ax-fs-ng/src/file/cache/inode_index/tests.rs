use super::*;

#[test]
fn late_hardlink_candidate_reuses_the_first_published_owner() {
    let mut index = InodeCacheIndex::new();
    let first = Arc::new(CachedFileShared::new_unbounded(1));
    let late = Arc::new(CachedFileShared::new_unbounded(1));
    let late_lifetime = Arc::downgrade(&late);
    // Both callers missed and constructed candidates before either published.
    let published = publish_in(&mut index, (1, 2), &first);
    first.set_len(37);
    let reused = publish_in(&mut index, (1, 2), &late);
    drop(late);

    assert!(Arc::ptr_eq(&published, &reused));
    assert_eq!(reused.len(), 37);
    assert!(late_lifetime.upgrade().is_none());
    assert!(Arc::ptr_eq(&index[&(1, 2)].upgrade().unwrap(), &first));
}

#[test]
fn expired_owner_can_be_replaced_without_aliasing_other_mounts() {
    let mut index = InodeCacheIndex::new();
    let expired = Arc::new(CachedFileShared::new_unbounded(1));
    drop(publish_in(&mut index, (1, 2), &expired));
    drop(expired);
    let next = Arc::new(CachedFileShared::new_unbounded(9));
    let other_mount = Arc::new(CachedFileShared::new_unbounded(17));

    assert!(Arc::ptr_eq(&publish_in(&mut index, (1, 2), &next), &next));
    assert!(Arc::ptr_eq(
        &publish_in(&mut index, (3, 2), &other_mount),
        &other_mount
    ));
    assert_eq!(index[&(1, 2)].upgrade().unwrap().len(), 9);
}
