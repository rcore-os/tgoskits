use super::*;

/// Split information bubbled upward during recursive insertion.
pub(super) struct SplitInfo {
    /// First logical block covered by the new right-hand node.
    pub(super) start_block: u32,
    /// Physical block storing the new right-hand node.
    pub(super) phy_block: AbsoluteBN,
}

impl<'a> ExtentTree<'a> {
    /// Returns the maximum number of entries that fit in one metadata block.
    pub(super) fn calc_block_eh_max() -> u16 {
        let hdr_size = Ext4ExtentHeader::disk_size();
        let entry_size = Ext4Extent::disk_size(); // Index and extent entries are both 12 bytes.
        (BLOCK_SIZE.saturating_sub(hdr_size) / entry_size) as u16
    }

    /// Returns the first logical block covered by a node.
    pub(super) fn get_node_start_block(node: &ExtentNode) -> u32 {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                if entries.is_empty() {
                    0
                } else {
                    entries[0].ee_block
                }
            }
            ExtentNode::Index { entries, .. } => {
                if entries.is_empty() {
                    0
                } else {
                    entries[0].ei_block
                }
            }
        }
    }
}
