//! Same-inode overlap observed inside real independent device reads.

use super::*;

#[test]
fn hardlink_readers_complete_same_and_disjoint_ranges_during_another_read() {
    for offset in [0, 8192] {
        let (filesystem, root, _) =
            super::super::sync_policy::background_mount(WritebackPolicy::empty());
        filesystem.lock().ext4.use_shared_device_cache().unwrap();
        let input = root
            .create(
                "input",
                NodeType::RegularFile,
                NodePermission::default(),
                0,
                0,
            )
            .unwrap();
        let bytes: Vec<_> = (0..12288).map(|index| (index % 251) as u8).collect();
        input
            .entry()
            .as_file()
            .unwrap()
            .write_at(&bytes, 0)
            .unwrap();
        let alias = root.link("alias", &input).unwrap();
        filesystem.sync_to_disk().unwrap();
        let input: Arc<Inode> = input.entry().downcast().unwrap();
        let alias: Arc<Inode> = alias.entry().downcast().unwrap();
        let number = InodeNumber::new(input.inode().try_into().unwrap()).unwrap();
        assert_eq!(input.inode(), alias.inode());
        assert_eq!(input.metadata().unwrap().nlink, 2);
        let probe = watch(&filesystem, &input, None, false);
        PROBE.with_borrow_mut(|slot| {
            slot.as_mut().unwrap().nested_read = Some(NestedRead {
                file: alias,
                offset,
                expected: bytes[offset as usize..offset as usize + 5].to_vec(),
                fail: false,
            });
        });
        let mut output = [0xa5; 5];

        assert_eq!(input.read_at(&mut output, 0), Ok(5));

        assert_eq!(&output, &bytes[..5]);
        PROBE.with_borrow(|slot| {
            let probe = slot.as_ref().unwrap();
            assert!(probe.observed > 0, "outer data read was not cold");
            assert!(probe.nested_read.is_none(), "nested read never ran");
        });
        drop(probe);
        assert!(filesystem.inode_access(number).try_write().is_some());
    }
}

#[test]
fn failed_nested_reader_does_not_release_the_outer_mapping_owner() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    let input = create_inode(&filesystem, b"input", b"hello");
    let number = InodeNumber::new(input.inode().try_into().unwrap()).unwrap();
    let alias = {
        let lifetime = filesystem.lock().retain_inode(&filesystem, number);
        Inode::new(lifetime, None)
    };
    let probe = watch(&filesystem, &input, None, false);
    PROBE.with_borrow_mut(|slot| {
        slot.as_mut().unwrap().nested_read = Some(NestedRead {
            file: alias,
            offset: 0,
            expected: b"hello".to_vec(),
            fail: true,
        });
    });
    let mut output = [0xa5; 5];

    assert_eq!(input.read_at(&mut output, 0), Ok(5));

    assert_eq!(&output, b"hello");
    PROBE.with_borrow(|slot| {
        let probe = slot.as_ref().unwrap();
        assert!(probe.observed >= 2, "nested failure missed real device I/O");
        assert!(probe.nested_read.is_none());
    });
    drop(probe);
    assert!(filesystem.inode_access(number).try_write().is_some());
    assert_eq!(input.write_at(b"after", 0), Ok(5));
}
