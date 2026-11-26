//! Kani harness for the extent tree node parse: it is total, and a node it accepts holds no more
//! entries than fit after its header. Behind `#[cfg(kani)]`, so only `cargo kani` compiles it.

use super::*;

/// **`ExtentTree::parse_node` is total on every slice of up to 24 bytes** (a header and one
/// entry): it never panics, and a node it accepts holds no more entries than fit in the bytes
/// after the header, and exactly as many as its header says.
///
/// Either guard alone holds the claim: the geometry check refuses a header whose `eh_max`
/// exceeds what fits, and the leaf loop refuses an entry that runs past the slice. Breaking the
/// claim takes dropping both, since dropping only one leaves the harness green.
#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn extent_node_parse_is_total() {
    let storage: [u8; 24] = kani::any();
    let length: usize = kani::any();
    kani::assume(length <= storage.len());
    let bytes = &storage[..length];
    let capacity = bytes.len().saturating_sub(12) / 12;
    if let Ok(node) = ExtentTree::parse_node(bytes) {
        let entries = match &node {
            ExtentNode::Leaf { entries, .. } => entries.len(),
            ExtentNode::Index { entries, .. } => entries.len(),
        };
        assert!(entries <= capacity);
        assert!(usize::from(node.header().eh_entries) == entries);
    }
}
