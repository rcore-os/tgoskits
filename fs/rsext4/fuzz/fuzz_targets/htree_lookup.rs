//! Mounts an image read-only and looks up every name it lists, and one it does not, in every
//! directory, so an indexed directory's hash-tree path parses its index blocks.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rsext4::{FileName, MountOptions};
use rsext4_fuzz::{mount, walk_directories};

fuzz_target!(|data: &[u8]| {
    let Some(mut filesystem) = mount(data, MountOptions::read_only_no_journal_replay()) else {
        return;
    };
    walk_directories(&mut filesystem, |filesystem, directory, entries| {
        for entry in entries.iter().take(256) {
            if let Ok(name) = FileName::new(&entry.name) {
                let _ = filesystem.lookup_child(directory, name);
            }
        }
        if let Ok(name) = FileName::new(b"not-in-the-directory") {
            let _ = filesystem.lookup_child(directory, name);
        }
    });
});
