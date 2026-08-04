//! Inode-local metadata mutation helpers.

use super::{
    Ext4DtimeUpdate, Ext4InodeMetadataUpdate, Ext4ModeUpdate,
    time::{get_now, resolve_time_spec},
};
use crate::{
    blockdev::{BlockDevice, Jbd2Dev},
    disknode::{Ext4Inode, Ext4Timestamp},
    error::{Ext4Error, Ext4Result},
    ext4::Ext4FileSystem,
};

impl Ext4FileSystem {
    pub(crate) fn inode_disk_size(&self) -> u16 {
        match self.superblock.s_inode_size {
            0 => Ext4Inode::LARGE_INODE_SIZE,
            size => size,
        }
    }

    pub(crate) fn default_inode_extra_isize(&self) -> u16 {
        let max_extra = Ext4Inode::max_extra_isize(self.inode_disk_size());
        let mut extra = core::cmp::min(self.superblock.s_want_extra_isize, max_extra);
        extra &= !3;
        extra
    }

    fn try_expand_extra_isize_for_field(&self, inode: &mut Ext4Inode, field_end: u16) -> bool {
        let inode_size = self.inode_disk_size();
        let max_extra = Ext4Inode::max_extra_isize(inode_size);
        let required = Ext4Inode::required_extra_isize(field_end);
        if required > max_extra {
            return false;
        }
        if inode.i_extra_isize >= required {
            return true;
        }

        let mut target = core::cmp::max(required, self.default_inode_extra_isize());
        target &= !3;
        if target > max_extra {
            return false;
        }

        inode.i_extra_isize = target;
        true
    }

    pub(crate) fn ensure_extra_isize_for_field(
        &self,
        inode: &mut Ext4Inode,
        field_end: u16,
    ) -> Ext4Result<()> {
        if self.try_expand_extra_isize_for_field(inode, field_end) {
            Ok(())
        } else {
            Err(Ext4Error::unsupported())
        }
    }

    /// Applies a prepared metadata update to an already loaded inode image.
    ///
    /// The update order mirrors Linux-style setattr handling: grow extra inode
    /// space for requested fields, apply identity and mode changes, resolve
    /// timestamps lazily from the device clock, and finally maintain `i_dtime`.
    pub(crate) fn apply_loaded_inode_metadata<B: BlockDevice>(
        &self,
        device: &Jbd2Dev<B>,
        inode: &mut Ext4Inode,
        update: Ext4InodeMetadataUpdate,
    ) -> Ext4Result<()> {
        let inode_size = self.inode_disk_size();

        // Grow `i_extra_isize` only for fields that are actually requested by this update.
        if update.atime.is_some() {
            let _ =
                self.try_expand_extra_isize_for_field(inode, Ext4Inode::FIELD_END_I_ATIME_EXTRA);
        }
        if update.mtime.is_some() {
            let _ =
                self.try_expand_extra_isize_for_field(inode, Ext4Inode::FIELD_END_I_MTIME_EXTRA);
        }
        if update.ctime.is_some() {
            let _ =
                self.try_expand_extra_isize_for_field(inode, Ext4Inode::FIELD_END_I_CTIME_EXTRA);
        }
        if update.crtime.is_some() {
            let _ =
                self.try_expand_extra_isize_for_field(inode, Ext4Inode::FIELD_END_I_CRTIME_EXTRA);
        }

        // Apply mode and ownership updates before timestamp maintenance.
        if let Some(mode) = update.mode {
            match mode {
                Ext4ModeUpdate::Replace(mode) => inode.set_mode_full(mode),
                Ext4ModeUpdate::Chmod(mode) => inode.set_mode_preserve_type(mode),
            }
        }

        if let Some(uid) = update.uid {
            inode.set_uid(uid);
        }
        if let Some(gid) = update.gid {
            inode.set_gid(gid);
        }

        if update.clear_suid_sgid_on_write {
            inode.clear_setid_bits_for_content_change();
        }
        if update.clear_suid_sgid_on_chown {
            inode.clear_setid_bits_for_chown();
        }

        if let Some(projid) = update.projid {
            self.ensure_extra_isize_for_field(inode, Ext4Inode::FIELD_END_I_PROJID)?;
            inode.i_projid = projid;
        }

        // Resolve `Now` only once even when multiple timestamps in the same update need it.
        let mut now_cache: Option<Ext4Timestamp> = None;

        if let Some(spec) = update.atime
            && let Some(ts) = resolve_time_spec(device, spec, &mut now_cache)?
        {
            inode.set_atime_ts(inode_size, ts);
        }

        if let Some(spec) = update.mtime
            && let Some(ts) = resolve_time_spec(device, spec, &mut now_cache)?
        {
            inode.set_mtime_ts(inode_size, ts);
        }

        if let Some(spec) = update.ctime
            && let Some(ts) = resolve_time_spec(device, spec, &mut now_cache)?
        {
            inode.set_ctime_ts(inode_size, ts);
        }

        if let Some(spec) = update.crtime
            && let Some(ts) = resolve_time_spec(device, spec, &mut now_cache)?
        {
            inode.set_crtime_ts(inode_size, ts);
        }

        // `i_dtime` is maintained separately because delete-time semantics do not match normal timestamps.
        match update.dtime {
            Ext4DtimeUpdate::Keep => {}
            Ext4DtimeUpdate::Clear => inode.i_dtime = 0,
            Ext4DtimeUpdate::SetNow => {
                let now = get_now(device, &mut now_cache)?;
                inode.i_dtime = if now.sec <= 0 { 0 } else { now.sec as u32 };
            }
        }

        Ok(())
    }
}
