//! Mounts an image read-write, which replays its journal when the image needs recovery, then
//! reads the root directory and unmounts.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rsext4::{DirectoryCursor, MountOptions};
use rsext4_fuzz::mount;

fuzz_target!(|data: &[u8]| {
    let Some(mut filesystem) = mount(data, MountOptions::read_write()) else {
        return;
    };
    let root = filesystem.root_inode();
    let _ = filesystem.read_directory(root, DirectoryCursor::Start, 64);
    let _ = filesystem.unmount();
});
