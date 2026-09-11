//! Named FIFO opens share the existing pipe buffer and endpoint accounting.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
use core::ops::Bound::{Excluded, Unbounded};

use axfs_ng_vfs::{FilesystemId, MountUseGuard};
use linux_raw_sys::general::{O_ACCMODE, O_NONBLOCK, O_RDONLY, O_RDWR, O_WRONLY};

use super::{Pipe, PipeAccess, PipeState, Shared};
use crate::{StarryError, StarryResult, file::File, sync::Mutex, task::UserTaskRef};

// The inode remains pinned by each opening/open file description. Weak entries
// never retain a buffer after the last endpoint and pending open have gone away.
type FifoKey = (FilesystemId, u64);

struct FifoRegistry {
    channels: BTreeMap<FifoKey, Weak<Shared>>,
    cleanup_after: Option<FifoKey>,
}

impl FifoRegistry {
    fn prune_stale(&mut self) {
        // Inspect two entries per open, advancing even past live channels.
        // This bounds lock work without allowing one long-lived first entry
        // to permanently shield all later stale entries from reclamation.
        for _ in 0..2 {
            let candidate = self
                .cleanup_after
                .and_then(|key| self.channels.range((Excluded(key), Unbounded)).next())
                .or_else(|| self.channels.first_key_value())
                .map(|(key, shared)| (*key, shared.strong_count() == 0));
            let Some((key, stale)) = candidate else {
                self.cleanup_after = None;
                break;
            };
            self.cleanup_after = Some(key);
            if stale {
                self.channels.remove(&key);
            }
        }
    }
}

static FIFOS: Mutex<FifoRegistry> = Mutex::new(FifoRegistry {
    channels: BTreeMap::new(),
    cleanup_after: None,
});

pub(super) struct NamedFile {
    pub(super) file: Arc<File>,
    _mount_use: MountUseGuard,
    // Linux suppresses HUP on a nonblocking reader until a writer has opened.
    pub(super) initial_writer_generation: Option<u64>,
}

impl Pipe {
    pub(crate) fn named_file(&self) -> Option<&Arc<File>> {
        self.named.as_ref().map(|named| &named.file)
    }

    /// Opens a FIFO endpoint, publishing it before waiting for its partner.
    pub(crate) fn open_fifo(
        task: &UserTaskRef,
        file: ax_fs_ng::File,
        flags: u32,
    ) -> StarryResult<Self> {
        let access = match flags & O_ACCMODE {
            O_RDONLY => PipeAccess::Read,
            O_WRONLY => PipeAccess::Write,
            O_RDWR => PipeAccess::ReadWrite,
            _ => return Err(StarryError::InvalidInput),
        };
        let nonblocking = flags & O_NONBLOCK != 0;
        let mount_use = file.location().mountpoint().acquire_use()?;
        let key = (
            file.location().mountpoint().filesystem_id(),
            file.location().entry().inode(),
        );
        let file = Arc::new(File::new(file, flags));
        let shared = {
            let mut registry = FIFOS.lock();
            registry.prune_stale();
            if let Some(shared) = registry.channels.get(&key).and_then(Weak::upgrade) {
                shared
            } else {
                registry.channels.remove(&key);
                if access == PipeAccess::Write && nonblocking {
                    return Err(StarryError::NoSuchDeviceOrAddress);
                }
                let shared = Arc::new(Shared::new(PipeState::empty()));
                registry.channels.insert(key, Arc::downgrade(&shared));
                shared
            }
        };
        let (wait_generation, initial_writer_generation) = shared.update_state(|state| {
            if access == PipeAccess::Write && nonblocking && state.readers == 0 {
                return Err(StarryError::NoSuchDeviceOrAddress);
            }
            let wait_generation = match access {
                PipeAccess::Read if !nonblocking && state.writers == 0 => {
                    Some(state.writer_generation)
                }
                PipeAccess::Write if state.readers == 0 => Some(state.reader_generation),
                _ => None,
            };
            let initial_writer_generation =
                (access == PipeAccess::Read && nonblocking && state.writers == 0)
                    .then_some(state.writer_generation);
            state.add_endpoint(access);
            Ok((wait_generation, initial_writer_generation))
        })?;
        // From here every cancellation, failed fd installation and final close
        // rolls back endpoint counts through the same Pipe destructor.
        let endpoint = Self {
            access,
            shared,
            non_blocking: core::sync::atomic::AtomicBool::new(nonblocking),
            named: Some(NamedFile {
                file,
                _mount_use: mount_use,
                initial_writer_generation,
            }),
        };
        endpoint.shared.open_wait.notify_all();
        if let Some(generation) = wait_generation {
            endpoint.wait_for_partner(task, generation)?;
        }
        Ok(endpoint)
    }

    fn wait_for_partner(&self, task: &UserTaskRef, generation: u64) -> StarryResult<()> {
        let partner_opened = || {
            let state = self.shared.state.lock();
            // A partner that opened and closed before this task ran still
            // completes the rendezvous. Current endpoint counts cannot show it.
            match self.access {
                PipeAccess::Read => state.writer_generation != generation,
                PipeAccess::Write => state.reader_generation != generation,
                PipeAccess::ReadWrite => true,
            }
        };
        loop {
            if partner_opened() {
                return Ok(());
            }
            if task.take_interrupt() {
                return Err(StarryError::Interrupted);
            }
            self.shared
                .open_wait
                .wait_until(|| partner_opened() || task.interrupted());
        }
    }
}
