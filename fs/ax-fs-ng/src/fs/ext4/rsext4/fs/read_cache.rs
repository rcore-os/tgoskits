//! Short mount visibility queries shared by file and directory read owners.

use alloc::{sync::Arc, vec::Vec};

use rsext4::{Ext4Result, InodeBlockRequest, InodeDataRequest, InodeReadCache};

use super::Ext4Filesystem;

pub(super) struct MountedReadCache<'a>(pub(super) &'a Ext4Filesystem);

impl InodeReadCache for MountedReadCache<'_> {
    fn visible(&mut self, request: &InodeBlockRequest) -> Ext4Result<Option<Arc<Vec<u8>>>> {
        self.0.lock().ext4.inode_read_block_image(request)
    }

    fn visible_data(
        &mut self,
        request: &InodeDataRequest,
    ) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>> {
        self.0.lock().ext4.inode_read_data_images(request)
    }
}
