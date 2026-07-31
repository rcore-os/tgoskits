use super::*;

mod delivery;
mod idle_pull;
mod lifecycle;
mod load_summary;
mod owner;
mod scheduler;

pub(crate) use idle_pull::IdlePullReservation;
pub use lifecycle::CpuLifecycleState;
pub(crate) use lifecycle::{CpuRemotePublication, CpuWakeCarrier};
pub use owner::CpuLocalOwnerBorrow;

/// Stable cross-CPU publication endpoint for one scheduler owner.
///
/// This object contains only atomic state and intrusive MPSC inboxes. It is
/// allocated separately from [`CpuLocal`], so remote producers never create a
/// shared reference to the owner-only runqueue object while its CPU holds a
/// unique mutable borrow.
#[derive(Debug)]
pub struct CpuRemote {
    owner: CpuId,
    owner_state: owner::OwnerState,
    publication: lifecycle::CpuPublicationState,
    scheduler: scheduler::SchedulerDoorbellState,
    load: load_summary::RemoteLoadState,
    idle_pull: idle_pull::IdlePullState,
    delivery: delivery::RemoteDeliveryState,
}

impl CpuRemote {
    pub(crate) fn create(owner: CpuId) -> Arc<Self> {
        Arc::new(Self {
            owner,
            owner_state: owner::OwnerState::new(),
            publication: lifecycle::CpuPublicationState::new(),
            scheduler: scheduler::SchedulerDoorbellState::new(),
            load: load_summary::RemoteLoadState::new(),
            idle_pull: idle_pull::IdlePullState::new(),
            delivery: delivery::RemoteDeliveryState::new(),
        })
    }
}

include!("remote/tests.rs");
