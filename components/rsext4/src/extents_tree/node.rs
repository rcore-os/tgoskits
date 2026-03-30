use super::*;

/// In-memory extent-tree node representation.
#[derive(Clone)]
pub enum ExtentNode {
    /// Leaf node with `eh_depth == 0` followed by `Ext4Extent` entries.
    Leaf {
        header: Ext4ExtentHeader,
        entries: Vec<Ext4Extent>,
    },
    /// Internal node with `eh_depth > 0` followed by `Ext4ExtentIdx` entries.
    Index {
        header: Ext4ExtentHeader,
        entries: Vec<Ext4ExtentIdx>,
    },
}

impl ExtentNode {
    pub fn header(&self) -> &Ext4ExtentHeader {
        match self {
            ExtentNode::Leaf { header, .. } => header,
            ExtentNode::Index { header, .. } => header,
        }
    }

    pub fn header_mut(&mut self) -> &mut Ext4ExtentHeader {
        match self {
            ExtentNode::Leaf { header, .. } => header,
            ExtentNode::Index { header, .. } => header,
        }
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self, ExtentNode::Leaf { .. })
    }
}
