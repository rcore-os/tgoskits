//! Filesystem-side metadata application helpers.

use super::{Ext4DtimeUpdate, Ext4InodeMetadataUpdate, Ext4MetadataReason};
use crate::{
    blockdev::{BlockDevice, Jbd2Dev},
    bmalloc::InodeNumber,
    disknode::{Ext4Inode, Ext4TimeSpec},
    error::{Ext4Error, Ext4Result},
    ext4::Ext4FileSystem,
    superblock::Ext4Superblock,
};

impl Ext4FileSystem {
    pub(crate) fn finalize_inode_update<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        inode: &mut Ext4Inode,
        update: Ext4InodeMetadataUpdate,
    ) -> Ext4Result<()> {
        self.apply_loaded_inode_metadata(device, inode, update)?;
        let updated = *inode;
        self.modify_inode(device, inode_num, |on_disk| *on_disk = updated)
    }

    pub(crate) fn apply_inode_metadata<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        update: Ext4InodeMetadataUpdate,
    ) -> Ext4Result<Ext4Inode> {
        let mut inode = self.get_inode_by_num(device, inode_num)?;
        self.finalize_inode_update(device, inode_num, &mut inode, update)?;
        Ok(inode)
    }

    pub(crate) fn apply_inode_flags<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        flags: u32,
    ) -> Ext4Result<Ext4Inode> {
        if flags & !Ext4Inode::EXT4_FL_USER_VISIBLE != 0 {
            return Err(Ext4Error::invalid_input());
        }

        let mut inode = self.get_inode_by_num(device, inode_num)?;
        let modifiable = Ext4Inode::mask_flags_for_mode(
            inode.i_mode,
            flags & Ext4Inode::EXT4_FL_USER_MODIFIABLE,
        );
        let preserved = inode.i_flags & !Ext4Inode::EXT4_FL_USER_MODIFIABLE;
        inode.i_flags = preserved | modifiable;
        self.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate {
                reason: Ext4MetadataReason::Flags,
                ctime: Some(Ext4TimeSpec::Now),
                ..Default::default()
            },
        )?;
        Ok(inode)
    }

    pub(crate) fn apply_inode_project<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        projid: u32,
    ) -> Ext4Result<Ext4Inode> {
        if !self
            .superblock
            .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT)
        {
            return Err(Ext4Error::unsupported());
        }

        let mut inode = self.get_inode_by_num(device, inode_num)?;
        self.ensure_extra_isize_for_field(&mut inode, Ext4Inode::FIELD_END_I_PROJID)?;
        inode.i_projid = projid;
        self.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate {
                reason: Ext4MetadataReason::Project,
                ctime: Some(Ext4TimeSpec::Now),
                ..Default::default()
            },
        )?;
        Ok(inode)
    }

    pub(crate) fn apply_inode_dtime<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        dtime: Ext4DtimeUpdate,
    ) -> Ext4Result<Ext4Inode> {
        let mut inode = self.get_inode_by_num(device, inode_num)?;
        self.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate {
                reason: Ext4MetadataReason::LinkCount,
                dtime,
                ..Default::default()
            },
        )?;
        Ok(inode)
    }

    pub(crate) fn touch_inode_atime_if_needed<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        let inode = self.get_inode_by_num(device, inode_num)?;
        if inode.i_flags & Ext4Inode::EXT4_NOATIME_FL != 0 {
            return Ok(());
        }

        let _ =
            self.apply_inode_metadata(device, inode_num, Ext4InodeMetadataUpdate::read_access())?;
        Ok(())
    }

    pub(crate) fn touch_parent_dir_for_entry_change<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        let _ = self.apply_inode_metadata(
            device,
            inode_num,
            Ext4InodeMetadataUpdate::parent_dir_change(),
        )?;
        Ok(())
    }

    pub(crate) fn touch_inode_ctime_for_link_change<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        let _ = self.apply_inode_metadata(
            device,
            inode_num,
            Ext4InodeMetadataUpdate::link_count_change(),
        )?;
        Ok(())
    }

    pub(crate) fn set_inode_links_count<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        links_count: u16,
    ) -> Ext4Result<Ext4Inode> {
        let mut inode = self.get_inode_by_num(device, inode_num)?;
        inode.i_links_count = links_count;
        self.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::link_count_change(),
        )?;
        Ok(inode)
    }
}
