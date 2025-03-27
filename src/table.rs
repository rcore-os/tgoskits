use alloc::{collections::btree_map::BTreeMap, sync::Arc};

use axsync::{Mutex, MutexGuard};

use crate::{Pgid, Pid, Process, ProcessGroup, Session, Sid};

static PROCESS_TABLE: Mutex<BTreeMap<Pid, Arc<Process>>> = Mutex::new(BTreeMap::new());
static PROCESS_GROUP_TABLE: Mutex<BTreeMap<Pgid, Arc<ProcessGroup>>> = Mutex::new(BTreeMap::new());
static SESSION_TABLE: Mutex<BTreeMap<Sid, Arc<Session>>> = Mutex::new(BTreeMap::new());

/// Get exclusive mutable access to the global [`Process`] table.
pub fn process_table() -> MutexGuard<'static, BTreeMap<Pid, Arc<Process>>> {
    PROCESS_TABLE.lock()
}

/// Get exclusive mutable access to the global [`ProcessGroup`] table.
pub fn process_group_table() -> MutexGuard<'static, BTreeMap<Pgid, Arc<ProcessGroup>>> {
    PROCESS_GROUP_TABLE.lock()
}

/// Get exclusive mutable access to the global [`Session`] table.
pub fn session_table() -> MutexGuard<'static, BTreeMap<Sid, Arc<Session>>> {
    SESSION_TABLE.lock()
}
