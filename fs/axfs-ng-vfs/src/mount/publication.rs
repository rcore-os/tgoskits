use super::*;

#[cfg(test)]
pub(super) mod test_hook;

/// Owns all temporary installation resources, including on failed publication.
///
/// Store this value before inspecting the paired result, and drop it only after
/// leaving every caller protection, including a namespace writer. It is not a
/// transaction: dropping it neither commits nor rolls back mount connections.
/// It cannot be cloned or reconstructed from a published mount.
#[must_use = "installation resources must be retired outside caller protections"]
pub struct MountPublicationRetirement {
    target: Location,
    traversal: propagation::RetainedLocations,
    propagation: propagation::PropagationRetirement,
    mount: Option<Arc<Mountpoint>>,
}

impl core::fmt::Debug for MountPublicationRetirement {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MountPublicationRetirement")
            .field("mount", &self.mount.as_ref().map(|mount| mount.mount_id()))
            .finish_non_exhaustive()
    }
}

impl MountPublicationRetirement {
    fn new(target: &Location) -> Self {
        Self {
            target: target.clone(),
            traversal: propagation::RetainedLocations::default(),
            propagation: propagation::PropagationRetirement::default(),
            mount: None,
        }
    }
}

impl Location {
    /// Installs a filesystem without an outer caller protection.
    ///
    /// This convenience entry releases temporary owners before returning.
    /// Use [`Self::mount_retiring`] during an allowed sleepable writer phase.
    pub fn mount(&self, fs: &Filesystem) -> VfsResult<Arc<Mountpoint>> {
        let (result, retirement) = self.mount_retiring(fs);
        drop(retirement);
        result
    }

    /// Installs a named source without an outer caller protection.
    ///
    /// Temporary owners retire inside this call, after topology exclusion.
    pub fn mount_with_source(&self, fs: &Filesystem, source: &str) -> VfsResult<Arc<Mountpoint>> {
        let (result, retirement) = self.mount_with_source_retiring(fs, source);
        drop(retirement);
        result
    }

    /// Installs a filesystem with a selected device number and source name.
    ///
    /// Do not call under outer protections: temporary owners retire before
    /// return. Use [`Self::mount_with_device_source_retiring`] in that case.
    pub fn mount_with_device_source(
        &self,
        fs: &Filesystem,
        device: u64,
        source: &str,
    ) -> VfsResult<Arc<Mountpoint>> {
        let (result, retirement) = self.mount_with_device_source_retiring(fs, device, source);
        drop(retirement);
        result
    }

    /// Installs a filesystem and returns successful or failed temporary owners.
    ///
    /// Uses the ordinary default source and device allocation. The caller must
    /// store the retirement before inspecting the result. Preparation may call
    /// the backend, allocate, or sleep; Scope, pin and IRQ-safe short locks are
    /// not permitted, even though an otherwise valid sleepable writer is.
    pub fn mount_retiring(
        &self,
        fs: &Filesystem,
    ) -> (VfsResult<Arc<Mountpoint>>, MountPublicationRetirement) {
        self.mount_with_source_retiring(fs, "none")
    }

    /// Installs a named source, retaining all temporary owners in the pair.
    ///
    /// Uses ordinary device allocation. The blocking and retirement boundaries
    /// are the same as [`Self::mount_retiring`], on both success and failure.
    pub fn mount_with_source_retiring(
        &self,
        fs: &Filesystem,
        source: &str,
    ) -> (VfsResult<Arc<Mountpoint>>, MountPublicationRetirement) {
        self.mount_with_device_source_retiring(
            fs,
            DEVICE_COUNTER.fetch_add(1, Ordering::Relaxed),
            source,
        )
    }

    /// Prepares and publishes one filesystem installation.
    ///
    /// The pair always owns the prepared mount, even if the target retired
    /// during backend preparation. Propagation retains its existing partial
    /// completion and error semantics; the retirement does not roll it back.
    ///
    /// Store the retirement before checking the result, and release it outside
    /// all caller gates. This may sleep, call the backend, and allocate: it is
    /// not valid under Scope/pin or an IRQ-safe State short lock.
    pub fn mount_with_device_source_retiring(
        &self,
        fs: &Filesystem,
        device: u64,
        source: &str,
    ) -> (VfsResult<Arc<Mountpoint>>, MountPublicationRetirement) {
        let mut retirement = MountPublicationRetirement::new(self);
        // Backend preparation stays outside topology. The carrier exists first
        // and owns the prepared mount before the first fallible validation.
        retirement.mount = Some(Mountpoint::new_with_device_source(
            fs,
            Some(retirement.target.clone()),
            device,
            source,
        ));
        let mount = retirement
            .mount
            .as_ref()
            .expect("mount was prepared")
            .clone();
        let result = (|| {
            let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
            self.mountpoint.check_topology_live_locked()?;
            let should_propagate = self.mountpoint.is_shared();
            self.check_is_dir()?;
            {
                let mut children = self.mountpoint.children.lock();
                if children.contains_key(&self.entry.key()) {
                    return Err(VfsError::ResourceBusy);
                }
                children.insert(self.entry.key(), mount.clone());
            }
            if should_propagate {
                Mountpoint::propagate_new_child_locked(
                    self.mountpoint(),
                    self,
                    &mount,
                    &mut retirement.propagation,
                )?;
            }
            MOUNT_TOPOLOGY_VERSION.fetch_add(1, Ordering::AcqRel);
            Ok(mount.clone())
        })();
        #[cfg(test)]
        test_hook::after_unlock(self.mountpoint(), &retirement);
        (result, retirement)
    }
}

impl Mountpoint {
    /// Attaches a detached tree without an outer caller protection.
    ///
    /// Parentless namespace anchors remain immovable. Temporary owners retire
    /// inside this call; use [`Self::attach_detached_retiring`] under a writer.
    pub fn attach_detached(self: &Arc<Self>, target: &Location) -> VfsResult<()> {
        let (result, retirement) = self.attach_detached_retiring(target);
        drop(retirement);
        result
    }

    /// Attaches a detached tree and returns all temporary owners on either path.
    ///
    /// The carrier retains this exact source before any validation. This keeps
    /// the ordinary error ordering and partial propagation semantics, without
    /// rollback on Drop. Save the retirement before checking the result and
    /// release it outside all caller protections.
    ///
    /// Directory checks, allocation, and propagation may block or call the
    /// backend. An allowed sleepable writer is not permission to hold Scope,
    /// pin or an IRQ-safe State lock during preparation.
    pub fn attach_detached_retiring(
        self: &Arc<Self>,
        target: &Location,
    ) -> (VfsResult<()>, MountPublicationRetirement) {
        let mut retirement = MountPublicationRetirement::new(target);
        retirement.mount = Some(self.clone());
        let result = (|| {
            let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
            self.check_topology_live_locked()?;
            target.mountpoint.check_topology_live_locked()?;
            if self.root_role() == MountRootRole::NamespaceAnchor
                || self.active_uses.lock().normally_unmounted
                || self.location.lock().is_some()
            {
                return Err(VfsError::InvalidInput);
            }
            if target.is_mountpoint() {
                return Err(VfsError::ResourceBusy);
            }
            target.check_is_dir()?;

            let root = retirement.traversal.push(self.root_location());
            let mut index = retirement.traversal.push(target.clone());
            loop {
                if retirement
                    .traversal
                    .get(index)
                    .ptr_eq(retirement.traversal.get(root))
                {
                    return Err(VfsError::FilesystemLoop);
                }
                match retirement.traversal.parent(index) {
                    Some(parent) => index = parent,
                    None => break,
                }
            }

            *self.root_role.lock() = MountRootRole::Ordinary;
            *self.location.lock() = Some(target.clone());
            target
                .mountpoint
                .children
                .lock()
                .insert(target.entry.key(), self.clone());
            if target.mountpoint.is_shared() {
                Self::propagate_new_child_locked(
                    target.mountpoint(),
                    target,
                    self,
                    &mut retirement.propagation,
                )?;
            }
            MOUNT_TOPOLOGY_VERSION.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })();
        #[cfg(test)]
        test_hook::after_unlock(target.mountpoint(), &retirement);
        (result, retirement)
    }
}
