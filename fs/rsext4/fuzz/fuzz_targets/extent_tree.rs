//! Parses one extent-tree node: the 60 bytes of an inode's `i_block`, or a whole index or leaf
//! block.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rsext4::extents_tree::ExtentTree;

fuzz_target!(|data: &[u8]| {
    let _ = ExtentTree::parse_node(data);
});
