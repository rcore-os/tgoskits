use super::*;

/// Mounts an ext4 filesystem from the given block device.
pub fn fs_mount<B: BlockDevice>(dev: &mut Jbd2Dev<B>) -> Ext4Result<Ext4FileSystem> {
    ext4::mount(dev)
}

/// Unmounts a previously mounted ext4 filesystem.
pub fn fs_umount<B: BlockDevice>(fs: Ext4FileSystem, dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
    ext4::umount(fs, dev)
}
