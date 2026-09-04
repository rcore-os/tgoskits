//! Stable PID identities and namespace-local number ownership.
//!
//! Scheduler task IDs never cross this boundary. A [`PidIdentity`] names one
//! generation; a [`PidNumber`] is only an index in one [`PidNamespace`].

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    fmt,
    future::poll_fn,
    marker::PhantomData,
    num::{NonZeroU32, NonZeroU64},
    sync::atomic::{AtomicU64, Ordering},
    task::Poll,
};

use ax_lazyinit::LazyLock;
use ax_task::{AxTaskRef, WeakAxTaskRef, future::block_on};
use axpoll::{IoEvents, PollSet};

use super::{Cred, Process, ProcessCpuTime, ProcessData, ProcessGroup, Session};
use crate::{StarryError, StarryResult, sync::IrqMutex};

static NEXT_IDENTITY_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_NAMESPACE_ID: AtomicU64 = AtomicU64::new(1);

/// Serializes reservation publication, identity removal, and shutdown.
static PUBLICATION_GATE: IrqMutex<()> = IrqMutex::new(());

/// A non-zero userspace PID number in one namespace.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PidNumber(NonZeroU32);

impl PidNumber {
    pub const fn new(number: u32) -> Option<Self> {
        match NonZeroU32::new(number) {
            Some(number) => Some(Self(number)),
            None => None,
        }
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Debug for PidNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(f)
    }
}

impl fmt::Display for PidNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(f)
    }
}

impl TryFrom<u32> for PidNumber {
    type Error = StarryError;

    fn try_from(number: u32) -> Result<Self, Self::Error> {
        Self::new(number).ok_or(StarryError::InvalidInput)
    }
}

impl From<PidNumber> for u32 {
    fn from(number: PidNumber) -> Self {
        number.get()
    }
}

macro_rules! define_pid_role_number {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name(PidNumber);

        impl $name {
            pub const fn get(self) -> u32 {
                self.0.get()
            }

            pub const fn pid_number(self) -> PidNumber {
                self.0
            }
        }

        impl From<PidNumber> for $name {
            fn from(number: PidNumber) -> Self {
                Self(number)
            }
        }

        impl From<$name> for PidNumber {
            fn from(number: $name) -> Self {
                number.pid_number()
            }
        }

        impl TryFrom<u32> for $name {
            type Error = StarryError;

            fn try_from(number: u32) -> Result<Self, Self::Error> {
                Ok(Self(PidNumber::try_from(number)?))
            }
        }

        impl From<$name> for u32 {
            fn from(number: $name) -> Self {
                number.get()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(f)
            }
        }
    };
}

define_pid_role_number!(TidNumber, "A namespace-local thread ID.");
define_pid_role_number!(TgidNumber, "A namespace-local thread-group ID.");
define_pid_role_number!(PgidNumber, "A namespace-local process-group ID.");
define_pid_role_number!(SidNumber, "A namespace-local session ID.");

/// A non-reusable kernel PID generation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PidIdentityId(NonZeroU64);

impl PidIdentityId {
    fn allocate() -> Self {
        let id = NEXT_IDENTITY_ID.fetch_add(1, Ordering::Relaxed);
        Self(NonZeroU64::new(id).expect("PID identity generation overflow"))
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for PidIdentityId {
    type Error = StarryError;

    fn try_from(id: u64) -> Result<Self, Self::Error> {
        NonZeroU64::new(id)
            .map(Self)
            .ok_or(StarryError::InvalidInput)
    }
}

/// A stable PID namespace generation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PidNamespaceId(NonZeroU64);

impl PidNamespaceId {
    fn allocate() -> Self {
        let id = NEXT_NAMESPACE_ID.fetch_add(1, Ordering::Relaxed);
        Self(NonZeroU64::new(id).expect("PID namespace generation overflow"))
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

pub type PidNamespaceRef = Arc<PidNamespace>;

/// One immutable local number in a PID identity's root-to-leaf binding chain.
pub struct PidBinding {
    namespace: PidNamespaceRef,
    number: PidNumber,
}

impl PidBinding {
    pub fn namespace(&self) -> &PidNamespaceRef {
        &self.namespace
    }

    pub const fn number(&self) -> PidNumber {
        self.number
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PidNamespaceLifecycle {
    AwaitingInit,
    Active,
    ShuttingDown,
    Dead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PidReservationKind {
    ProcessLeader,
    Thread,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PidSlotState {
    Reserved,
    Published,
}

struct PidSlot {
    identity_id: PidIdentityId,
    state: PidSlotState,
    identity: Option<Arc<PidIdentity>>,
}

struct PidNamespaceState {
    lifecycle: PidNamespaceLifecycle,
    init_identity: Option<PidIdentityId>,
    reserved_init: Option<PidIdentityId>,
    /// Cyclic allocation cursor, not the lowest currently available number.
    next_number: u32,
    by_number: BTreeMap<PidNumber, PidSlot>,
    by_identity: BTreeMap<PidIdentityId, PidNumber>,
}

/// Immutable namespace topology plus its synchronized number slots.
pub struct PidNamespace {
    id: PidNamespaceId,
    level: u32,
    parent: Option<PidNamespaceRef>,
    state: IrqMutex<PidNamespaceState>,
    task_exit_event: PollSet,
}

pub static ROOT_PID_NS: LazyLock<PidNamespaceRef> =
    LazyLock::new(|| Arc::new(PidNamespace::new_root()));

impl PidNamespace {
    fn new_root() -> Self {
        Self::new(None, PidNamespaceLifecycle::Active)
    }

    fn new(parent: Option<PidNamespaceRef>, lifecycle: PidNamespaceLifecycle) -> Self {
        let level = parent.as_ref().map_or(0, |parent| {
            parent
                .level
                .checked_add(1)
                .expect("PID namespace level overflow")
        });
        Self {
            id: PidNamespaceId::allocate(),
            level,
            parent,
            state: IrqMutex::new(PidNamespaceState {
                lifecycle,
                init_identity: None,
                reserved_init: None,
                next_number: 1,
                by_number: BTreeMap::new(),
                by_identity: BTreeMap::new(),
            }),
            task_exit_event: PollSet::new(),
        }
    }

    pub fn new_child(parent: PidNamespaceRef) -> PidNamespaceRef {
        Arc::new(Self::new(Some(parent), PidNamespaceLifecycle::AwaitingInit))
    }

    pub const fn id(&self) -> PidNamespaceId {
        self.id
    }

    pub const fn level(&self) -> u32 {
        self.level
    }

    pub fn parent(&self) -> Option<PidNamespaceRef> {
        self.parent.clone()
    }

    pub fn lifecycle(&self) -> PidNamespaceLifecycle {
        self.state.lock().lifecycle
    }

    pub fn init_identity(&self) -> Option<PidIdentityId> {
        self.state.lock().init_identity
    }

    pub fn lookup(&self, number: PidNumber) -> Option<Arc<PidIdentity>> {
        let _publication = PUBLICATION_GATE.lock();
        let state = self.state.lock();
        let slot = state.by_number.get(&number)?;
        (slot.state == PidSlotState::Published)
            .then(|| slot.identity.clone())
            .flatten()
            .filter(|identity| identity.is_lookup_visible())
    }

    /// Resolve one stable generation without reinterpreting a reusable number.
    pub fn lookup_identity(&self, identity_id: PidIdentityId) -> Option<Arc<PidIdentity>> {
        let _publication = PUBLICATION_GATE.lock();
        let state = self.state.lock();
        let number = *state.by_identity.get(&identity_id)?;
        let slot = state.by_number.get(&number)?;
        (slot.identity_id == identity_id && slot.state == PidSlotState::Published)
            .then(|| slot.identity.clone())
            .flatten()
            .filter(|identity| identity.is_lookup_visible())
    }

    fn reserve(
        &self,
        identity_id: PidIdentityId,
        kind: PidReservationKind,
        namespace_init: bool,
    ) -> StarryResult<PidNumber> {
        let mut state = self.state.lock();
        if state.by_identity.contains_key(&identity_id) {
            return Err(StarryError::AlreadyExists);
        }
        let may_reserve = match state.lifecycle {
            PidNamespaceLifecycle::Active => {
                !namespace_init
                    || (self.level == 0
                        && state.init_identity.is_none()
                        && state.reserved_init.is_none())
            }
            PidNamespaceLifecycle::AwaitingInit => {
                namespace_init
                    && kind == PidReservationKind::ProcessLeader
                    && state.reserved_init.is_none()
            }
            PidNamespaceLifecycle::ShuttingDown | PidNamespaceLifecycle::Dead => {
                return Err(StarryError::NoMemory);
            }
        };
        if !may_reserve {
            return Err(StarryError::InvalidInput);
        }

        let number = state.allocate_number()?;
        if namespace_init {
            if number.get() != 1 {
                return Err(StarryError::BadState);
            }
            state.reserved_init = Some(identity_id);
        }
        state.by_identity.insert(identity_id, number);
        state.by_number.insert(
            number,
            PidSlot {
                identity_id,
                state: PidSlotState::Reserved,
                identity: None,
            },
        );
        Ok(number)
    }

    fn validate_publish(&self, identity_id: PidIdentityId, number: PidNumber) -> StarryResult<()> {
        let state = self.state.lock();
        if matches!(
            state.lifecycle,
            PidNamespaceLifecycle::ShuttingDown | PidNamespaceLifecycle::Dead
        ) {
            return Err(StarryError::NoMemory);
        }
        let slot = state.by_number.get(&number).ok_or(StarryError::BadState)?;
        if slot.identity_id != identity_id || slot.state != PidSlotState::Reserved {
            return Err(StarryError::BadState);
        }
        Ok(())
    }

    fn publish(&self, identity: &Arc<PidIdentity>, number: PidNumber) {
        let mut state = self.state.lock();
        let slot = state
            .by_number
            .get_mut(&number)
            .expect("validated PID reservation disappeared");
        assert_eq!(slot.identity_id, identity.id);
        assert_eq!(slot.state, PidSlotState::Reserved);
        slot.state = PidSlotState::Published;
        slot.identity = Some(identity.clone());
        if state.reserved_init == Some(identity.id) {
            state.reserved_init = None;
            state.init_identity = Some(identity.id);
            state.lifecycle = PidNamespaceLifecycle::Active;
        }
    }

    fn rollback(&self, identity_id: PidIdentityId, number: PidNumber) -> bool {
        let mut state = self.state.lock();
        let matches = state.by_number.get(&number).is_some_and(|slot| {
            slot.identity_id == identity_id && slot.state == PidSlotState::Reserved
        });
        if !matches {
            return false;
        }
        state.by_number.remove(&number);
        assert_eq!(state.by_identity.remove(&identity_id), Some(number));
        if state.reserved_init == Some(identity_id) {
            state.reserved_init = None;
            // PID namespace init must remain PID 1 when its unpublished
            // reservation is retried.
            state.next_number = 1;
        }
        true
    }

    fn remove(&self, identity_id: PidIdentityId, number: PidNumber) {
        let mut state = self.state.lock();
        if state.lifecycle == PidNamespaceLifecycle::Dead && !state.by_number.contains_key(&number)
        {
            return;
        }
        let slot = state
            .by_number
            .get(&number)
            .expect("published PID slot disappeared before detach");
        assert_eq!(slot.identity_id, identity_id);
        assert_eq!(slot.state, PidSlotState::Published);
        state.by_number.remove(&number);
        assert_eq!(state.by_identity.remove(&identity_id), Some(number));
        if state.lifecycle == PidNamespaceLifecycle::ShuttingDown && state.by_number.is_empty() {
            state.lifecycle = PidNamespaceLifecycle::Dead;
        }
    }

    pub fn begin_shutdown(&self, init: PidIdentityId) -> Option<PidNamespaceShutdown<'_>> {
        let _publication = PUBLICATION_GATE.lock();
        let mut state = self.state.lock();
        if self.level == 0
            || state.lifecycle != PidNamespaceLifecycle::Active
            || state.init_identity != Some(init)
        {
            return None;
        }
        state.lifecycle = PidNamespaceLifecycle::ShuttingDown;
        Some(PidNamespaceShutdown {
            namespace: self,
            init,
        })
    }

    pub fn published_members(&self) -> Vec<Arc<PidIdentity>> {
        let _publication = PUBLICATION_GATE.lock();
        self.state
            .lock()
            .by_number
            .values()
            .filter(|slot| slot.state == PidSlotState::Published)
            .filter_map(|slot| slot.identity.clone())
            .collect()
    }

    fn finish_shutdown(&self, init: PidIdentityId) {
        let _publication = PUBLICATION_GATE.lock();
        let mut state = self.state.lock();
        assert_eq!(state.lifecycle, PidNamespaceLifecycle::ShuttingDown);
        assert_eq!(state.init_identity, Some(init));
        state.by_number.clear();
        state.by_identity.clear();
        state.reserved_init = None;
        state.lifecycle = PidNamespaceLifecycle::Dead;
    }
}

/// Explicit blocking phase for PID namespace init shutdown.
pub struct PidNamespaceShutdown<'a> {
    namespace: &'a PidNamespace,
    init: PidIdentityId,
}

impl PidNamespaceShutdown<'_> {
    pub fn wait_for_live_descendants(&self) {
        let has_live_descendants = || {
            self.namespace
                .published_members()
                .into_iter()
                .any(|identity| identity.id() != self.init && identity.live_task().is_some())
        };
        if has_live_descendants() {
            block_on(poll_fn(|cx| {
                if !has_live_descendants() {
                    return Poll::Ready(());
                }
                unsafe {
                    self.namespace
                        .task_exit_event
                        .register(cx.waker(), IoEvents::IN)
                };
                if has_live_descendants() {
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            }));
        }
        self.namespace.finish_shutdown(self.init);
    }
}

impl Drop for PidNamespaceShutdown<'_> {
    fn drop(&mut self) {
        debug_assert!(matches!(
            self.namespace.lifecycle(),
            PidNamespaceLifecycle::ShuttingDown | PidNamespaceLifecycle::Dead
        ));
    }
}

impl PidNamespaceState {
    fn allocate_number(&mut self) -> StarryResult<PidNumber> {
        let start = self.next_number.max(1);
        let mut candidate = start;
        loop {
            let number = PidNumber::new(candidate).ok_or(StarryError::NoMemory)?;
            if !self.by_number.contains_key(&number) {
                self.next_number = candidate.checked_add(1).unwrap_or(1);
                return Ok(number);
            }
            candidate = candidate.checked_add(1).unwrap_or(1);
            if candidate == start {
                return Err(StarryError::NoMemory);
            }
        }
    }
}

pub fn pid_namespace_lineage(innermost: &PidNamespaceRef) -> Vec<PidNamespaceRef> {
    let mut lineage = Vec::new();
    let mut current = Some(innermost.clone());
    while let Some(namespace) = current {
        current = namespace.parent();
        lineage.push(namespace);
    }
    lineage.reverse();
    lineage
}

/// Whole-chain number ownership before task publication.
#[must_use = "dropping an unpublished reservation rolls back every number slot"]
pub struct PidReservation {
    identity_id: PidIdentityId,
    bindings: Vec<(PidNamespaceRef, PidNumber)>,
    identity: Arc<PidIdentity>,
    published: bool,
}

impl PidReservation {
    pub fn reserve(target: &PidNamespaceRef, kind: PidReservationKind) -> StarryResult<Self> {
        let identity_id = PidIdentityId::allocate();
        let lineage = pid_namespace_lineage(target);
        let _publication = PUBLICATION_GATE.lock();
        let mut bindings = Vec::new();
        bindings
            .try_reserve_exact(lineage.len())
            .map_err(|_| StarryError::NoMemory)?;
        for namespace in lineage {
            let state = namespace.state.lock();
            let namespace_init = Arc::ptr_eq(&namespace, target)
                && (state.lifecycle == PidNamespaceLifecycle::AwaitingInit
                    || (namespace.level == 0
                        && state.init_identity.is_none()
                        && state.reserved_init.is_none()));
            drop(state);
            match namespace.reserve(identity_id, kind, namespace_init) {
                Ok(number) => bindings.push((namespace, number)),
                Err(error) => {
                    for (reserved_namespace, number) in bindings.iter().rev() {
                        assert!(reserved_namespace.rollback(identity_id, *number));
                    }
                    return Err(error);
                }
            }
        }
        let identity_bindings: Arc<[PidBinding]> = bindings
            .iter()
            .map(|(namespace, number)| PidBinding {
                namespace: namespace.clone(),
                number: *number,
            })
            .collect::<Vec<_>>()
            .into();
        let identity = Arc::new(PidIdentity {
            id: identity_id,
            bindings: identity_bindings,
            state: IrqMutex::new(PidIdentityState {
                publication: PidIdentityPublication::Reserved,
                runtime: RuntimeTaskLink::Reserved,
                roles: 0,
                process: None,
                process_group: Weak::new(),
                session: Weak::new(),
                process_lifecycle: ProcessLifecycle::None,
                process_exit_event: None,
            }),
        });
        Ok(Self {
            identity_id,
            bindings,
            identity,
            published: false,
        })
    }

    pub fn number_in(&self, observer: &PidNamespaceRef) -> Option<PidNumber> {
        self.bindings
            .iter()
            .find(|(namespace, _)| Arc::ptr_eq(namespace, observer))
            .map(|(_, number)| *number)
    }

    /// Returns the still-unpublished stable identity for suspended task setup.
    ///
    /// The identity may own role leases and prepared topology while reserved,
    /// but namespace lookup cannot observe it until [`Self::publish`] commits
    /// the whole binding chain.
    pub fn identity(&self) -> Arc<PidIdentity> {
        self.identity.clone()
    }

    pub fn publish(mut self) -> StarryResult<Arc<PidIdentity>> {
        let _publication = PUBLICATION_GATE.lock();
        for (namespace, number) in &self.bindings {
            namespace.validate_publish(self.identity_id, *number)?;
        }
        for (namespace, number) in &self.bindings {
            namespace.publish(&self.identity, *number);
        }
        self.identity.state.lock().publication = PidIdentityPublication::Published;
        self.published = true;
        Ok(self.identity.clone())
    }
}

impl Drop for PidReservation {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let _publication = PUBLICATION_GATE.lock();
        for (namespace, number) in self.bindings.iter().rev() {
            assert!(namespace.rollback(self.identity_id, *number));
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PidIdentityPublication {
    Reserved,
    Published,
    Detached,
}

enum RuntimeTaskLink {
    Reserved,
    Live(WeakAxTaskRef),
    Exited,
}

struct PidIdentityState {
    publication: PidIdentityPublication,
    runtime: RuntimeTaskLink,
    roles: u8,
    process: Option<Arc<Process>>,
    process_group: Weak<ProcessGroup>,
    session: Weak<Session>,
    process_lifecycle: ProcessLifecycle,
    process_exit_event: Option<Arc<PollSet>>,
}

pub(super) enum ProcessLifecycle {
    None,
    Live(Weak<ProcessData>),
    Zombie(ZombieSnapshot),
    Reaping,
    Reaped,
}

impl ProcessLifecycle {
    fn is_publicly_resolvable(&self) -> bool {
        matches!(self, Self::Live(_) | Self::Zombie(_))
    }
}

/// Immutable process-exit data retained until one consuming wait reaps it.
pub(crate) struct ZombieSnapshot {
    pub(crate) cred: Arc<Cred>,
    pub(crate) ptrace_tracer: Option<PidSnapshot>,
    pub(crate) is_clone_child: bool,
    pub(crate) wait_parent_tid: TidNumber,
    pub(crate) cpu_time: ProcessCpuTime,
    pub(crate) tgid_lease: PidRoleLease<Tgid>,
}

/// A generation-stable identity analogous to Linux `struct pid`.
pub struct PidIdentity {
    id: PidIdentityId,
    bindings: Arc<[PidBinding]>,
    state: IrqMutex<PidIdentityState>,
}

impl PidIdentity {
    fn is_lookup_visible(&self) -> bool {
        let state = self.state.lock();
        state.publication == PidIdentityPublication::Published
            && (matches!(state.runtime, RuntimeTaskLink::Live(_))
                || matches!(
                    state.process_lifecycle,
                    ProcessLifecycle::Live(_) | ProcessLifecycle::Zombie(_)
                )
                || state.roles & ((1 << 2) | (1 << 3)) != 0)
    }

    pub const fn id(&self) -> PidIdentityId {
        self.id
    }

    pub fn bindings(&self) -> &[PidBinding] {
        &self.bindings
    }

    pub fn active_namespace(&self) -> PidNamespaceRef {
        self.bindings
            .last()
            .expect("published identity has no PID binding")
            .namespace
            .clone()
    }

    pub fn visible_number(&self, observer: &PidNamespaceRef) -> Option<PidNumber> {
        if observer.lifecycle() == PidNamespaceLifecycle::Dead {
            return None;
        }
        self.bindings
            .iter()
            .find(|binding| Arc::ptr_eq(&binding.namespace, observer))
            .map(|binding| binding.number)
    }

    pub(crate) fn visible_number_in(&self, observer: PidNamespaceId) -> Option<PidNumber> {
        self.bindings
            .iter()
            .find(|binding| binding.namespace.id == observer)
            .map(|binding| binding.number)
    }

    pub(crate) fn root_number(&self) -> PidNumber {
        self.bindings
            .first()
            .expect("published identity has no root PID binding")
            .number
    }

    pub fn nspid_chain(&self, observer: &PidNamespaceRef) -> Option<Vec<PidNumber>> {
        let first = self
            .bindings
            .iter()
            .position(|binding| Arc::ptr_eq(&binding.namespace, observer))?;
        Some(
            self.bindings[first..]
                .iter()
                .map(|binding| binding.number)
                .collect(),
        )
    }

    pub fn attach_task(&self, task: &AxTaskRef) {
        let mut state = self.state.lock();
        assert_eq!(state.publication, PidIdentityPublication::Published);
        assert!(matches!(state.runtime, RuntimeTaskLink::Reserved));
        state.runtime = RuntimeTaskLink::Live(Arc::downgrade(task));
    }

    pub fn live_task(&self) -> Option<AxTaskRef> {
        let state = self.state.lock();
        let RuntimeTaskLink::Live(task) = &state.runtime else {
            return None;
        };
        task.upgrade()
    }

    pub fn mark_task_exited(&self) {
        let should_detach = {
            let mut state = self.state.lock();
            state.runtime = RuntimeTaskLink::Exited;
            state.roles == 0
        };
        if should_detach {
            self.detach();
        }
        for binding in self.bindings.iter().rev() {
            unsafe { binding.namespace.task_exit_event.wake(IoEvents::IN) };
        }
    }

    /// Transfers this identity's live runtime link during `de_thread`.
    pub fn transfer_task(&self, task: &AxTaskRef) {
        let mut state = self.state.lock();
        assert_eq!(state.publication, PidIdentityPublication::Published);
        assert!(matches!(
            state.runtime,
            RuntimeTaskLink::Live(_) | RuntimeTaskLink::Exited
        ));
        state.runtime = RuntimeTaskLink::Live(Arc::downgrade(task));
    }

    /// Attaches the process lifecycle to the leader identity exactly once.
    pub(super) fn bind_process(
        &self,
        process: Arc<Process>,
        exit_event: Arc<PollSet>,
        process_data: Weak<ProcessData>,
    ) {
        let mut state = self.state.lock();
        assert!(self.has_role_locked::<Tgid>(&state));
        assert!(state.process.is_none());
        assert!(matches!(state.process_lifecycle, ProcessLifecycle::None));
        state.process = Some(process);
        state.process_exit_event = Some(exit_event);
        state.process_lifecycle = ProcessLifecycle::Live(process_data);
    }

    pub(super) fn bind_process_group(&self, group: &Arc<ProcessGroup>) {
        let mut state = self.state.lock();
        assert!(self.has_role_locked::<Pgid>(&state));
        if let Some(existing) = state.process_group.upgrade() {
            assert!(Arc::ptr_eq(&existing, group));
        } else {
            state.process_group = Arc::downgrade(group);
        }
    }

    pub(super) fn bind_session(&self, session: &Arc<Session>) {
        let mut state = self.state.lock();
        assert!(self.has_role_locked::<Sid>(&state));
        if let Some(existing) = state.session.upgrade() {
            assert!(Arc::ptr_eq(&existing, session));
        } else {
            state.session = Arc::downgrade(session);
        }
    }

    pub(crate) fn process_group(&self) -> Option<Arc<ProcessGroup>> {
        self.state.lock().process_group.upgrade()
    }

    pub(crate) fn session(&self) -> Option<Arc<Session>> {
        self.state.lock().session.upgrade()
    }

    pub(crate) fn process(&self) -> Arc<Process> {
        self.state
            .lock()
            .process
            .clone()
            .expect("TGID identity has no process topology")
    }

    pub(crate) fn live_data(&self) -> Option<Arc<ProcessData>> {
        let state = self.state.lock();
        if state.publication != PidIdentityPublication::Published {
            return None;
        }
        let ProcessLifecycle::Live(process_data) = &state.process_lifecycle else {
            return None;
        };
        process_data.upgrade()
    }

    pub(crate) fn is_zombie(&self) -> bool {
        matches!(
            self.state.lock().process_lifecycle,
            ProcessLifecycle::Zombie(_)
        )
    }

    pub(crate) fn is_exited(&self) -> bool {
        matches!(
            self.state.lock().process_lifecycle,
            ProcessLifecycle::Zombie(_) | ProcessLifecycle::Reaping | ProcessLifecycle::Reaped
        )
    }

    pub(crate) fn is_reaped(&self) -> bool {
        matches!(
            self.state.lock().process_lifecycle,
            ProcessLifecycle::Reaping | ProcessLifecycle::Reaped
        )
    }

    pub(crate) fn public_process(&self) -> StarryResult<Arc<Process>> {
        let state = self.state.lock();
        if state.publication == PidIdentityPublication::Published
            && state.process_lifecycle.is_publicly_resolvable()
        {
            state.process.clone().ok_or(StarryError::BadState)
        } else {
            Err(StarryError::NoSuchProcess)
        }
    }

    pub(crate) fn process_poll_events(&self) -> IoEvents {
        let state = self.state.lock();
        match &state.process_lifecycle {
            ProcessLifecycle::None | ProcessLifecycle::Live(_) => IoEvents::empty(),
            ProcessLifecycle::Zombie(_) => IoEvents::IN | IoEvents::RDNORM,
            ProcessLifecycle::Reaping | ProcessLifecycle::Reaped => {
                IoEvents::IN | IoEvents::RDNORM | IoEvents::HUP
            }
        }
    }

    pub(crate) fn process_exit_event(&self) -> Arc<PollSet> {
        self.state
            .lock()
            .process_exit_event
            .clone()
            .expect("TGID identity has no process exit event")
    }

    pub(crate) fn matches_process(&self, process: &Process) -> bool {
        self.state
            .lock()
            .process
            .as_deref()
            .is_some_and(|registered| core::ptr::eq(registered, process))
    }

    pub(crate) fn publish_zombie(
        &self,
        expected: &Arc<ProcessData>,
        zombie: ZombieSnapshot,
    ) -> Result<(), ZombieSnapshot> {
        let mut state = self.state.lock();
        let matches = matches!(
            &state.process_lifecycle,
            ProcessLifecycle::Live(process_data)
                if process_data
                    .upgrade()
                    .is_some_and(|registered| Arc::ptr_eq(&registered, expected))
        );
        if !matches {
            return Err(zombie);
        }
        state.process_lifecycle = ProcessLifecycle::Zombie(zombie);
        Ok(())
    }

    pub(crate) fn claim_reap(&self, expected: &Arc<Process>) -> Option<ZombieSnapshot> {
        if !self.matches_process(expected) {
            return None;
        }
        let mut state = self.state.lock();
        let ProcessLifecycle::Zombie(zombie) =
            core::mem::replace(&mut state.process_lifecycle, ProcessLifecycle::Reaping)
        else {
            return None;
        };
        Some(zombie)
    }

    pub(crate) fn finish_reap(&self) {
        let process = {
            let mut state = self.state.lock();
            assert!(matches!(state.process_lifecycle, ProcessLifecycle::Reaping));
            state.process_lifecycle = ProcessLifecycle::Reaped;
            state.process.take()
        };
        // Reaped identities no longer own process topology. Drop outside the
        // identity lock: the final Process may release PGID/SID role leases,
        // whose destructors re-enter this identity to detach its PID slots.
        drop(process);
    }

    pub(crate) fn zombie_snapshot<R>(&self, f: impl FnOnce(&ZombieSnapshot) -> R) -> Option<R> {
        let state = self.state.lock();
        let ProcessLifecycle::Zombie(zombie) = &state.process_lifecycle else {
            return None;
        };
        Some(f(zombie))
    }

    #[cfg(all(test, not(axtest)))]
    pub(super) fn bind_zombie_for_test(
        &self,
        process: Arc<Process>,
        exit_event: Arc<PollSet>,
        zombie: ZombieSnapshot,
    ) {
        let mut state = self.state.lock();
        assert!(matches!(state.process_lifecycle, ProcessLifecycle::None));
        state.process = Some(process);
        state.process_exit_event = Some(exit_event);
        state.process_lifecycle = ProcessLifecycle::Zombie(zombie);
    }

    pub fn acquire_role<R: PidRole>(self: &Arc<Self>) -> StarryResult<PidRoleLease<R>> {
        let mut state = self.state.lock();
        if state.publication == PidIdentityPublication::Detached {
            return Err(StarryError::BadState);
        }
        if state.roles & R::BIT != 0 {
            return Err(StarryError::AlreadyExists);
        }
        state.roles |= R::BIT;
        Ok(PidRoleLease {
            identity: Some(Arc::downgrade(self)),
            role: PhantomData,
        })
    }

    pub fn has_role<R: PidRole>(&self) -> bool {
        self.has_role_locked::<R>(&self.state.lock())
    }

    fn has_role_locked<R: PidRole>(&self, state: &PidIdentityState) -> bool {
        state.roles & R::BIT != 0
    }

    pub fn snapshot(&self) -> PidSnapshot {
        PidSnapshot {
            identity_id: self.id,
            bindings: self
                .bindings
                .iter()
                .map(|binding| (binding.namespace.id, binding.number))
                .collect::<Vec<_>>()
                .into(),
        }
    }

    fn release_role<R: PidRole>(&self) {
        let should_detach = {
            let mut state = self.state.lock();
            assert_ne!(state.roles & R::BIT, 0, "PID role released twice");
            state.roles &= !R::BIT;
            state.roles == 0 && matches!(state.runtime, RuntimeTaskLink::Exited)
        };
        if should_detach {
            self.detach();
        }
    }

    fn detach(&self) {
        let _publication = PUBLICATION_GATE.lock();
        {
            let mut state = self.state.lock();
            if state.publication == PidIdentityPublication::Detached {
                return;
            }
            assert_eq!(state.roles, 0, "PID detached with a live role");
            assert!(matches!(state.runtime, RuntimeTaskLink::Exited));
            state.publication = PidIdentityPublication::Detached;
        }
        for binding in self.bindings.iter().rev() {
            binding.namespace.remove(self.id, binding.number);
        }
    }

    /// Non-blocking RAII fallback for a clone that published its PID identity
    /// but failed before the scheduler retained a live task reference.
    ///
    /// Normal clone completion never calls this transition. If a task is
    /// already scheduler-owned, the fallback deliberately leaves it alone;
    /// scheduler/task exit then performs the ordinary ordered release.
    pub(crate) fn abort_failed_task_publication(&self) {
        let _publication = PUBLICATION_GATE.lock();
        {
            let mut state = self.state.lock();
            if state.publication != PidIdentityPublication::Published {
                return;
            }
            let scheduler_owns_task = match &state.runtime {
                RuntimeTaskLink::Reserved | RuntimeTaskLink::Exited => false,
                RuntimeTaskLink::Live(task) => task.upgrade().is_some(),
            };
            if scheduler_owns_task {
                return;
            }
            if !matches!(
                &state.process_lifecycle,
                ProcessLifecycle::None | ProcessLifecycle::Live(_)
            ) {
                return;
            }
            state.runtime = RuntimeTaskLink::Exited;
            state.publication = PidIdentityPublication::Detached;
            state.process_lifecycle = ProcessLifecycle::Reaped;
        }
        for binding in self.bindings.iter().rev() {
            binding.namespace.remove(self.id, binding.number);
        }
    }
}

pub trait PidRole: Send + Sync + 'static {
    const BIT: u8;
}

macro_rules! define_role {
    ($name:ident, $bit:expr) => {
        pub enum $name {}

        impl PidRole for $name {
            const BIT: u8 = $bit;
        }
    };
}

define_role!(Tid, 1 << 0);
define_role!(Tgid, 1 << 1);
define_role!(Pgid, 1 << 2);
define_role!(Sid, 1 << 3);

/// Unique RAII ownership of one identity role.
pub struct PidRoleLease<R: PidRole> {
    identity: Option<Weak<PidIdentity>>,
    role: PhantomData<R>,
}

impl<R: PidRole> PidRoleLease<R> {
    pub fn identity(&self) -> Arc<PidIdentity> {
        self.identity
            .as_ref()
            .expect("inactive PID role lease")
            .upgrade()
            .expect("PID role outlived its published identity")
    }

    pub fn release(mut self) {
        self.identity
            .take()
            .expect("PID role released twice")
            .upgrade()
            .expect("PID role outlived its published identity")
            .release_role::<R>();
    }
}

impl<R: PidRole> Drop for PidRoleLease<R> {
    fn drop(&mut self) {
        if let Some(identity) = self.identity.take() {
            identity
                .upgrade()
                .expect("PID role outlived its published identity")
                .release_role::<R>();
        }
    }
}

/// Historical PID data that does not retain namespace number slots.
#[derive(Clone, Debug)]
pub struct PidSnapshot {
    identity_id: PidIdentityId,
    bindings: Arc<[(PidNamespaceId, PidNumber)]>,
}

impl PidSnapshot {
    pub const fn identity_id(&self) -> PidIdentityId {
        self.identity_id
    }

    pub fn visible_number(&self, observer: PidNamespaceId) -> Option<PidNumber> {
        self.bindings
            .iter()
            .find(|(namespace, _)| *namespace == observer)
            .map(|(_, number)| *number)
    }
}

/// A fixed observer namespace used for typed PID resolution and projection.
#[derive(Clone)]
pub struct PidView {
    observer: PidNamespaceRef,
}

impl PidView {
    pub fn new(observer: PidNamespaceRef) -> Self {
        Self { observer }
    }

    pub fn resolve_identity(&self, number: PidNumber) -> StarryResult<Arc<PidIdentity>> {
        self.observer
            .lookup(number)
            .ok_or(StarryError::NoSuchProcess)
    }

    pub fn resolve_thread(&self, number: TidNumber) -> StarryResult<Arc<PidIdentity>> {
        let identity = self.resolve_role::<Tid>(number.pid_number())?;
        identity
            .live_task()
            .is_some()
            .then_some(identity)
            .ok_or(StarryError::NoSuchProcess)
    }

    pub fn resolve_process(&self, number: TgidNumber) -> StarryResult<Arc<PidIdentity>> {
        let identity = self.resolve_role::<Tgid>(number.pid_number())?;
        identity.public_process()?;
        Ok(identity)
    }

    pub fn resolve_group(&self, number: PgidNumber) -> StarryResult<Arc<ProcessGroup>> {
        self.resolve_role::<Pgid>(number.pid_number())?
            .process_group()
            .ok_or(StarryError::NoSuchProcess)
    }

    pub fn resolve_session(&self, number: SidNumber) -> StarryResult<Arc<Session>> {
        self.resolve_role::<Sid>(number.pid_number())?
            .session()
            .ok_or(StarryError::NoSuchProcess)
    }

    fn resolve_role<R: PidRole>(&self, number: PidNumber) -> StarryResult<Arc<PidIdentity>> {
        let identity = self.resolve_identity(number)?;
        identity
            .has_role::<R>()
            .then_some(identity)
            .ok_or(StarryError::NoSuchProcess)
    }

    pub fn visible_number(&self, identity: &PidIdentity) -> Option<PidNumber> {
        (self.observer.lifecycle() != PidNamespaceLifecycle::Dead)
            .then(|| identity.visible_number_in(self.observer.id()))
            .flatten()
    }

    pub fn visible_snapshot_number(&self, snapshot: &PidSnapshot) -> Option<PidNumber> {
        snapshot.visible_number(self.observer.id())
    }

    pub fn visible_thread_number(&self, identity: &PidIdentity) -> Option<TidNumber> {
        identity
            .has_role::<Tid>()
            .then(|| self.visible_number(identity).map(TidNumber::from))
            .flatten()
    }

    pub fn visible_process_number(&self, identity: &PidIdentity) -> Option<TgidNumber> {
        identity
            .has_role::<Tgid>()
            .then(|| self.visible_number(identity).map(TgidNumber::from))
            .flatten()
    }

    pub fn visible_group_number(&self, identity: &PidIdentity) -> Option<PgidNumber> {
        identity
            .has_role::<Pgid>()
            .then(|| self.visible_number(identity).map(PgidNumber::from))
            .flatten()
    }

    pub fn visible_session_number(&self, identity: &PidIdentity) -> Option<SidNumber> {
        identity
            .has_role::<Sid>()
            .then(|| self.visible_number(identity).map(SidNumber::from))
            .flatten()
    }

    pub fn nspid_chain(&self, identity: &PidIdentity) -> Option<Vec<PidNumber>> {
        (self.observer.lifecycle() != PidNamespaceLifecycle::Dead)
            .then(|| identity.nspid_chain(&self.observer))
            .flatten()
    }
}

#[cfg(all(test, not(axtest)))]
fn pid_identity_state_machine_rules_hold_for_test() -> bool {
    let root = Arc::new(PidNamespace::new_root());
    let root_init = PidReservation::reserve(&root, PidReservationKind::ProcessLeader)
        .unwrap()
        .publish()
        .unwrap();
    let root_init_tid = root_init.acquire_role::<Tid>().unwrap();
    let root_init_tgid = root_init.acquire_role::<Tgid>().unwrap();

    let child = PidNamespace::new_child(root.clone());
    let first = PidReservation::reserve(&child, PidReservationKind::ProcessLeader).unwrap();
    let first_root_number = first.number_in(&root).unwrap();
    let first_child_number = first.number_in(&child).unwrap();
    drop(first);
    let second = PidReservation::reserve(&child, PidReservationKind::ProcessLeader).unwrap();
    let second_root_number = second.number_in(&root).unwrap();
    if second_root_number == first_root_number
        || second.number_in(&child) != Some(first_child_number)
    {
        return false;
    }
    let prepared_child_init = second.identity();
    let child_tid = prepared_child_init.acquire_role::<Tid>().unwrap();
    if root.lookup(first_root_number).is_some() || child.lookup(first_child_number).is_some() {
        return false;
    }
    let child_init = second.publish().unwrap();
    if !Arc::ptr_eq(&prepared_child_init, &child_init) {
        return false;
    }

    let child_tgid = child_init.acquire_role::<Tgid>().unwrap();
    let session = Session::new(child_init.clone()).unwrap();
    let group = ProcessGroup::get_or_create(child_init.clone(), &session).unwrap();
    let view = PidView::new(child.clone());
    let root_view = PidView::new(root.clone());
    if !Arc::ptr_eq(
        &view
            .resolve_group(PgidNumber::from(first_child_number))
            .unwrap(),
        &group,
    ) || !Arc::ptr_eq(
        &view
            .resolve_session(SidNumber::from(first_child_number))
            .unwrap(),
        &session,
    ) || view.nspid_chain(&child_init) != Some(alloc::vec![first_child_number])
        || root_view.nspid_chain(&child_init)
            != Some(alloc::vec![second_root_number, first_child_number])
    {
        return false;
    }

    let group_only = PidReservation::reserve(&child, PidReservationKind::Thread)
        .unwrap()
        .publish()
        .unwrap();
    let group_number = group_only.visible_number(&child).unwrap();
    let group_only_pgid = group_only.acquire_role::<Pgid>().unwrap();
    if view.resolve_role::<Pgid>(group_number).is_err()
        || !matches!(
            view.resolve_role::<Tgid>(group_number),
            Err(StarryError::NoSuchProcess)
        )
        || !matches!(
            view.resolve_role::<Tid>(group_number),
            Err(StarryError::NoSuchProcess)
        )
        || view.visible_group_number(&group_only) != Some(PgidNumber::from(group_number))
        || view.visible_process_number(&group_only).is_some()
    {
        return false;
    }
    group_only.mark_task_exited();
    group_only_pgid.release();

    child_init.mark_task_exited();
    drop(group);
    drop(session);
    child_tid.release();
    child_tgid.release();

    let old = PidReservation::reserve(&root, PidReservationKind::Thread)
        .unwrap()
        .publish()
        .unwrap();
    let old_number = old.root_number();
    let old_generation = old.id();
    let old_role = old.acquire_role::<Pgid>().unwrap();
    old.mark_task_exited();
    old_role.release();
    if root.lookup_identity(old_generation).is_some() {
        return false;
    }
    let replacement = PidReservation::reserve(&root, PidReservationKind::Thread)
        .unwrap()
        .publish()
        .unwrap();
    let replacement_role = replacement.acquire_role::<Pgid>().unwrap();
    let generation_is_stable = replacement.root_number() != old_number
        && replacement.id() != old_generation
        && old.id() == old_generation
        && root.lookup_identity(old_generation).is_none()
        && root
            .lookup_identity(replacement.id())
            .is_some_and(|registered| Arc::ptr_eq(&registered, &replacement));
    replacement.mark_task_exited();
    replacement_role.release();

    let failed_reservation = PidReservation::reserve(&root, PidReservationKind::Thread).unwrap();
    let failed_identity = failed_reservation.identity();
    let failed_role = failed_identity.acquire_role::<Pgid>().unwrap();
    let failed_number = failed_identity.root_number();
    let failed_identity = failed_reservation.publish().unwrap();
    if root.lookup(failed_number).is_none() {
        return false;
    }
    failed_identity.abort_failed_task_publication();
    let failed_publication_was_removed = root.lookup(failed_number).is_none();
    failed_role.release();

    root_init.mark_task_exited();
    root_init_tid.release();
    root_init_tgid.release();
    generation_is_stable && failed_publication_was_removed
}

#[cfg(all(test, axtest))]
fn pid_namespace_descendant_shutdown_waits_for_runtime_exit_for_test() -> bool {
    let root = Arc::new(PidNamespace::new_root());
    let root_init = PidReservation::reserve(&root, PidReservationKind::ProcessLeader)
        .unwrap()
        .publish()
        .unwrap();
    let root_tid = root_init.acquire_role::<Tid>().unwrap();
    let root_tgid = root_init.acquire_role::<Tgid>().unwrap();
    let child = PidNamespace::new_child(root);
    let child_init = PidReservation::reserve(&child, PidReservationKind::ProcessLeader)
        .unwrap()
        .publish()
        .unwrap();
    let child_tid = child_init.acquire_role::<Tid>().unwrap();
    let child_tgid = child_init.acquire_role::<Tgid>().unwrap();
    let view = PidView::new(child.clone());

    let descendant = PidReservation::reserve(&child, PidReservationKind::Thread)
        .unwrap()
        .publish()
        .unwrap();
    let descendant_tid = descendant.acquire_role::<Tid>().unwrap();
    let release_descendant = Arc::new(core::sync::atomic::AtomicBool::new(false));
    let descendant_body = descendant.clone();
    let release_descendant_body = release_descendant.clone();
    let descendant_task = ax_task::spawn(move || {
        while !release_descendant_body.load(Ordering::Acquire) {
            ax_task::yield_now();
        }
        descendant_body.mark_task_exited();
        descendant_tid.release();
    });
    descendant.attach_task(&descendant_task);

    let shutdown = child.begin_shutdown(child_init.id()).unwrap();
    let rejects_new_descendants = matches!(
        PidReservation::reserve(&child, PidReservationKind::Thread),
        Err(StarryError::NoMemory)
    );
    release_descendant.store(true, Ordering::Release);
    shutdown.wait_for_live_descendants();
    let descendant_exit = descendant_task.join() == 0;
    let init_hidden = view.visible_number(&child_init).is_none()
        && view.nspid_chain(&child_init).is_none();

    child_init.mark_task_exited();
    child_tid.release();
    child_tgid.release();
    drop(shutdown);
    let namespace_dead = child.lifecycle() == PidNamespaceLifecycle::Dead;
    root_init.mark_task_exited();
    root_tid.release();
    root_tgid.release();

    rejects_new_descendants && descendant_exit && init_hidden && namespace_dead
}

#[cfg(all(test, not(axtest)))]
pub(super) fn new_test_pid_namespace() -> PidNamespaceRef {
    Arc::new(PidNamespace::new_root())
}

#[cfg(all(test, not(axtest)))]
pub(super) fn new_test_process_identity(
    namespace: &PidNamespaceRef,
) -> (Arc<PidIdentity>, PidRoleLease<Tgid>) {
    let identity = PidReservation::reserve(namespace, PidReservationKind::ProcessLeader)
        .unwrap()
        .publish()
        .unwrap();
    let tgid = identity.acquire_role::<Tgid>().unwrap();
    (identity, tgid)
}

#[cfg(test)]
mod tests {
    #[cfg(all(test, not(axtest)))]
    extern crate std;

    #[cfg(all(test, not(axtest)))]
    use alloc::vec;

    #[cfg(all(test, not(axtest)))]
    use super::*;

    #[cfg(all(test, not(axtest)))]
    fn root_process() -> (
        PidNamespaceRef,
        Arc<PidIdentity>,
        PidRoleLease<Tid>,
        PidRoleLease<Tgid>,
    ) {
        let root = Arc::new(PidNamespace::new_root());
        let identity = PidReservation::reserve(&root, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let tid = identity.acquire_role::<Tid>().unwrap();
        let tgid = identity.acquire_role::<Tgid>().unwrap();
        (root, identity, tid, tgid)
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn released_pid_does_not_move_cyclic_cursor_backwards() {
        let (root, _root_init, _root_tid, _root_tgid) = root_process();
        let first = PidReservation::reserve(&root, PidReservationKind::Thread)
            .unwrap()
            .publish()
            .unwrap();
        let first_tid = first.acquire_role::<Tid>().unwrap();
        let first_number = first.root_number();
        first.mark_task_exited();
        first_tid.release();

        let second = PidReservation::reserve(&root, PidReservationKind::Thread)
            .unwrap()
            .publish()
            .unwrap();
        let second_tid = second.acquire_role::<Tid>().unwrap();
        assert_eq!(second.root_number().get(), first_number.get() + 1);
        second.mark_task_exited();
        second_tid.release();
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn multi_namespace_reservation_drop_preserves_cyclic_cursors() {
        let (root, _root_init, _root_tid, _root_tgid) = root_process();
        let child = PidNamespace::new_child(root.clone());
        let first = PidReservation::reserve(&child, PidReservationKind::ProcessLeader).unwrap();
        let root_number = first.number_in(&root).unwrap();
        let child_number = first.number_in(&child).unwrap();
        drop(first);

        let second = PidReservation::reserve(&child, PidReservationKind::ProcessLeader).unwrap();
        assert_eq!(second.number_in(&root).unwrap().get(), root_number.get() + 1);
        // A failed namespace-init reservation is the one exception: retrying
        // that unpublished transaction must still allocate PID 1.
        assert_eq!(second.number_in(&child), Some(child_number));
        assert!(root.lookup(root_number).is_none());
        assert!(child.lookup(child_number).is_none());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn reservation_is_invisible_until_publication() {
        let (root, _root_init, _root_tid, _root_tgid) = root_process();
        let reservation = PidReservation::reserve(&root, PidReservationKind::Thread).unwrap();
        let number = reservation.number_in(&root).unwrap();
        let prepared = reservation.identity();
        let pgid = prepared.acquire_role::<Pgid>().unwrap();
        assert!(root.lookup(number).is_none());
        let identity = reservation.publish().unwrap();
        assert!(Arc::ptr_eq(&prepared, &identity));
        assert_eq!(root.lookup(number).unwrap().id(), identity.id());
        identity.mark_task_exited();
        pgid.release();
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn pidfd_arc_resists_number_reuse_after_cyclic_wrap() {
        let (root, _root_init, _root_tid, _root_tgid) = root_process();
        let identity = PidReservation::reserve(&root, PidReservationKind::Thread)
            .unwrap()
            .publish()
            .unwrap();
        let tid = identity.acquire_role::<Tid>().unwrap();
        identity.state.lock().runtime = RuntimeTaskLink::Live(WeakAxTaskRef::new());
        let number = identity.visible_number(&root).unwrap();
        let old_identity_id = identity.id();
        let pidfd_identity = identity.clone();
        identity.mark_task_exited();
        tid.release();
        assert!(root.lookup(number).is_none());
        assert!(root.lookup_identity(old_identity_id).is_none());

        // Model allocator wrap without exhausting the full PID number space.
        root.state.lock().next_number = number.get();
        let replacement = PidReservation::reserve(&root, PidReservationKind::Thread)
            .unwrap()
            .publish()
            .unwrap();
        let replacement_tid = replacement.acquire_role::<Tid>().unwrap();
        replacement.state.lock().runtime = RuntimeTaskLink::Live(WeakAxTaskRef::new());
        assert_eq!(replacement.visible_number(&root), Some(number));
        assert_ne!(replacement.id(), pidfd_identity.id());
        assert!(Arc::ptr_eq(
            &root.lookup_identity(replacement.id()).unwrap(),
            &replacement
        ));
        assert!(root.lookup_identity(old_identity_id).is_none());
        replacement.mark_task_exited();
        replacement_tid.release();
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn live_process_stays_visible_after_its_leader_runtime_exits() {
        let (root, _root_init, _root_tid, _root_tgid) = root_process();
        let identity = PidReservation::reserve(&root, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let tid = identity.acquire_role::<Tid>().unwrap();
        let tgid = identity.acquire_role::<Tgid>().unwrap();
        let number = identity.root_number();
        {
            let mut state = identity.state.lock();
            state.runtime = RuntimeTaskLink::Exited;
            state.process_lifecycle = ProcessLifecycle::Live(Weak::new());
        }
        assert!(
            root.lookup(number)
                .is_some_and(|found| Arc::ptr_eq(&found, &identity))
        );

        identity.state.lock().process_lifecycle = ProcessLifecycle::Reaped;
        assert!(root.lookup(number).is_none());
        tid.release();
        tgid.release();
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn role_typed_views_reject_a_number_with_the_wrong_role() {
        let (root, root_identity, root_tid, root_tgid) = root_process();
        let identity = PidReservation::reserve(&root, PidReservationKind::Thread)
            .unwrap()
            .publish()
            .unwrap();
        let pgid = identity.acquire_role::<Pgid>().unwrap();
        let number = identity.root_number();
        let view = PidView::new(root);

        assert!(view.resolve_role::<Pgid>(number).is_ok());
        assert!(matches!(
            view.resolve_role::<Tid>(number),
            Err(StarryError::NoSuchProcess)
        ));
        assert!(matches!(
            view.resolve_role::<Tgid>(number),
            Err(StarryError::NoSuchProcess)
        ));
        assert!(matches!(
            view.resolve_role::<Sid>(number),
            Err(StarryError::NoSuchProcess)
        ));
        assert_eq!(view.visible_thread_number(&identity), None);
        assert_eq!(view.visible_process_number(&identity), None);
        assert_eq!(
            view.visible_group_number(&identity),
            Some(PgidNumber::from(number))
        );
        assert_eq!(view.visible_session_number(&identity), None);

        identity.mark_task_exited();
        pgid.release();
        root_identity.mark_task_exited();
        root_tid.release();
        root_tgid.release();
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn observer_view_projects_each_namespace_number_and_hides_dead_namespace() {
        let (root, _root_init, _root_tid, _root_tgid) = root_process();
        let child = PidNamespace::new_child(root.clone());
        let identity = PidReservation::reserve(&child, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let tid = identity.acquire_role::<Tid>().unwrap();
        let tgid = identity.acquire_role::<Tgid>().unwrap();
        let root_number = identity.visible_number(&root).unwrap();
        let child_number = identity.visible_number(&child).unwrap();
        let root_view = PidView::new(root);
        let child_view = PidView::new(child.clone());

        assert_eq!(
            root_view.visible_process_number(&identity),
            Some(TgidNumber::from(root_number))
        );
        assert_eq!(
            child_view.visible_process_number(&identity),
            Some(TgidNumber::from(child_number))
        );
        assert_eq!(
            root_view.nspid_chain(&identity),
            Some(vec![root_number, child_number])
        );
        assert_eq!(child_view.nspid_chain(&identity), Some(vec![child_number]));

        let shutdown = child.begin_shutdown(identity.id()).unwrap();
        child.finish_shutdown(identity.id());
        assert_eq!(child.lifecycle(), PidNamespaceLifecycle::Dead);
        assert_eq!(child_view.visible_number(&identity), None);
        assert_eq!(child_view.nspid_chain(&identity), None);
        identity.mark_task_exited();
        tid.release();
        tgid.release();
        drop(shutdown);
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn pgid_and_sid_roles_keep_a_reaped_number_published() {
        let (root, identity, tid, tgid) = root_process();
        let number = identity.visible_number(&root).unwrap();
        let pgid = identity.acquire_role::<Pgid>().unwrap();
        let sid = identity.acquire_role::<Sid>().unwrap();
        identity.mark_task_exited();
        tid.release();
        tgid.release();
        assert!(root.lookup(number).is_some());
        pgid.release();
        assert!(root.lookup(number).is_some());
        sid.release();
        assert!(root.lookup(number).is_none());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn namespace_shutdown_rejects_new_reservations() {
        let (root, _root_init, _root_tid, _root_tgid) = root_process();
        let child = PidNamespace::new_child(root);
        let init = PidReservation::reserve(&child, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let tid = init.acquire_role::<Tid>().unwrap();
        let tgid = init.acquire_role::<Tgid>().unwrap();
        let shutdown = child.begin_shutdown(init.id()).unwrap();
        assert!(matches!(
            PidReservation::reserve(&child, PidReservationKind::ProcessLeader),
            Err(StarryError::NoMemory)
        ));
        child.finish_shutdown(init.id());
        init.mark_task_exited();
        tid.release();
        tgid.release();
        drop(shutdown);
        assert_eq!(child.lifecycle(), PidNamespaceLifecycle::Dead);
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn pid_identity_state_machine_rules_hold() {
        assert!(super::pid_identity_state_machine_rules_hold_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn pid_namespace_descendant_shutdown_waits_for_runtime_exit() {
        assert!(super::pid_namespace_descendant_shutdown_waits_for_runtime_exit_for_test());
    }
}
