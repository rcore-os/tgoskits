//! Validate mount observations against the actual publication exclusion.

use core::sync::atomic::Ordering;

use super::{MOUNT_TOPOLOGY_MUTATION, MOUNT_TOPOLOGY_VERSION, MountObservation};
use crate::{VfsError, VfsResult};

impl MountObservation {
    pub fn capture() -> VfsResult<Self> {
        let _topology = MOUNT_TOPOLOGY_MUTATION
            .try_lock()
            .ok_or(VfsError::WouldBlock)?;
        Ok(Self(MOUNT_TOPOLOGY_VERSION.load(Ordering::Acquire)))
    }

    pub fn validate(self) -> VfsResult<()> {
        let current = Self::capture()?;
        if current.0 == self.0 {
            Ok(())
        } else {
            Err(VfsError::WouldBlock)
        }
    }
}
