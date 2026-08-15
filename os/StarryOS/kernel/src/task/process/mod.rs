//! Process Management

mod group;
mod relations;
mod session;
mod topology;

pub use group::ProcessGroup;
pub(crate) use relations::{
    ChildRelations, GroupMembers, GroupMoveScope, ProcessRelationTxn, RelationLock, SessionGroups,
    ensure_session_capacity,
};
pub use session::Session;
#[cfg(axtest)]
pub(crate) use session::duplicate_live_session_identity_is_rejected_for_test;
pub use topology::{Process, ProcessCpuTime, ThreadExit, init_proc};
