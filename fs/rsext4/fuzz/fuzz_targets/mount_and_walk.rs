//! Mounts an image read-only and reads everything a bounded walk reaches: each directory, each
//! inode's metadata, extents and extended attributes, and the start of each file.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rsext4::MountOptions;
use rsext4_fuzz::{mount, read_inode_fully, walk_directories};

fuzz_target!(|data: &[u8]| {
    let Some(mut filesystem) = mount(data, MountOptions::read_only_no_journal_replay()) else {
        return;
    };
    let _ = filesystem.statfs();
    walk_directories(&mut filesystem, |filesystem, _directory, entries| {
        for entry in entries {
            read_inode_fully(filesystem, entry.inode);
        }
    });
    let _ = filesystem.unmount();
});
