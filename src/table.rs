use alloc::{collections::btree_map::BTreeMap, sync::Arc};

use kspin::{SpinNoIrq, SpinNoIrqGuard};

use crate::{Pgid, Pid, Process, ProcessGroup, Session, Sid};

static PROCESS_TABLE: SpinNoIrq<BTreeMap<Pid, Arc<Process>>> = SpinNoIrq::new(BTreeMap::new());
static PROCESS_GROUP_TABLE: SpinNoIrq<BTreeMap<Pgid, Arc<ProcessGroup>>> =
    SpinNoIrq::new(BTreeMap::new());
static SESSION_TABLE: SpinNoIrq<BTreeMap<Sid, Arc<Session>>> = SpinNoIrq::new(BTreeMap::new());

/// Get exclusive mutable access to the global [`Process`] table.
pub fn process_table() -> SpinNoIrqGuard<'static, BTreeMap<Pid, Arc<Process>>> {
    PROCESS_TABLE.lock()
}

/// Get exclusive mutable access to the global [`ProcessGroup`] table.
pub fn process_group_table() -> SpinNoIrqGuard<'static, BTreeMap<Pgid, Arc<ProcessGroup>>> {
    PROCESS_GROUP_TABLE.lock()
}

/// Get exclusive mutable access to the global [`Session`] table.
pub fn session_table() -> SpinNoIrqGuard<'static, BTreeMap<Sid, Arc<Session>>> {
    SESSION_TABLE.lock()
}
