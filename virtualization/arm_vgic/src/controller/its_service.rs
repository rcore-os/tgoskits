//! Sleepable task-side software-ITS state.
//!
//! The software ITS owns per-VM dynamic guest-programmable state: the register
//! model and the device/collection/translation tables. Guest command-queue
//! copies and command decoding are task-context work, so this state is guarded
//! by a sleepable mutex and is never entered from a native raw delivery path.
//! The native controller receives only pre-decoded [`crate::ItsAction`] delivery
//! effects, which it applies under its own short raw lock before the wake is
//! published outside every lock.

use alloc::{collections::BTreeMap, sync::Arc};

use ax_sync::Mutex;
use axdevice_base::ItsId;

use crate::{GicV3MmioRegion, GuestMemory, ItsState};

/// Task-side software-ITS service retained by the full controller.
pub(crate) struct ItsService {
    memory: Option<Arc<dyn GuestMemory>>,
    states: Mutex<BTreeMap<ItsId, ItsState>>,
}

impl ItsService {
    /// Builds one empty ITS state per configured instance.
    ///
    /// The guest-memory capability may be an empty per-run adapter that is
    /// bound once before hardware entry; only its presence is required here so
    /// a guest-visible ITS can never be constructed without a boundable memory
    /// capability.
    pub(crate) fn new(
        memory: Option<Arc<dyn GuestMemory>>,
        instances: &[(ItsId, GicV3MmioRegion)],
    ) -> Self {
        let states = instances
            .iter()
            .map(|(id, _)| (*id, ItsState::new()))
            .collect();
        Self {
            memory,
            states: Mutex::new(states),
        }
    }

    /// Returns the sleepable ITS state map.
    ///
    /// The map is the task-side software-ITS register and translation model for
    /// this VM, so its guarantee is expressed directly by the returned
    /// [`Mutex`] rather than through a lock alias.
    pub(crate) fn states(&self) -> &Mutex<BTreeMap<ItsId, ItsState>> {
        &self.states
    }

    /// Returns the checked guest-memory capability when one is installed.
    pub(crate) fn memory(&self) -> Option<&dyn GuestMemory> {
        self.memory.as_deref()
    }
}
