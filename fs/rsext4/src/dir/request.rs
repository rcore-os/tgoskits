//! Internal requests shared by resolved-parent namespace mutations.

use super::FileName;
use crate::bmalloc::InodeNumber;

/// Metadata shared by typed inode-creation primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CreateEntryRequest<'a> {
    pub parent: InodeNumber,
    pub name: FileName<'a>,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
}
