//! Shared metadata timestamp helpers.

use crate::{
    blockdev::{BlockDevice, Jbd2Dev},
    disknode::{Ext4TimeSpec, Ext4Timestamp},
    error::Ext4Result,
};

pub(crate) fn get_now<B: BlockDevice>(
    device: &Jbd2Dev<B>,
    now_cache: &mut Option<Ext4Timestamp>,
) -> Ext4Result<Ext4Timestamp> {
    if let Some(now) = *now_cache {
        return Ok(now);
    }

    let now = device.current_time()?;
    *now_cache = Some(now);
    Ok(now)
}

pub(crate) fn resolve_time_spec<B: BlockDevice>(
    device: &Jbd2Dev<B>,
    spec: Ext4TimeSpec,
    now_cache: &mut Option<Ext4Timestamp>,
) -> Ext4Result<Option<Ext4Timestamp>> {
    match spec {
        Ext4TimeSpec::Omit => Ok(None),
        Ext4TimeSpec::Set(ts) => Ok(Some(ts)),
        Ext4TimeSpec::Now => Ok(Some(get_now(device, now_cache)?)),
    }
}
