//! OS-independent runtime capabilities used by the ext4 core.

use core::time::Duration;

use crate::{
    disknode::Ext4Timestamp,
    error::{Ext4Error, Ext4Result},
};

/// Wall-clock capability used for on-disk inode timestamps.
pub trait Clock {
    fn now(&self) -> Ext4Result<Ext4Timestamp>;
}

/// Cryptographically suitable entropy supplied by the embedding runtime.
pub trait EntropySource {
    fn fill_bytes(&mut self, output: &mut [u8]) -> Ext4Result<()>;
}

impl EntropySource for () {
    fn fill_bytes(&mut self, _output: &mut [u8]) -> Ext4Result<()> {
        Err(Ext4Error::unsupported_capability("runtime:entropy"))
    }
}

/// Blocking delay used only by MMP's synchronous mount-time ownership check.
///
/// Periodic MMP scheduling remains the embedding runtime's responsibility. The
/// core returns the required refresh interval and never creates a task or owns
/// an OS timer.
pub trait Delay {
    fn wait(&mut self, duration: Duration) -> Ext4Result<()>;
}

impl Delay for () {
    fn wait(&mut self, _duration: Duration) -> Ext4Result<()> {
        Err(Ext4Error::unsupported_capability("runtime:delay"))
    }
}

/// Pure identity data recorded in the ext4 MMP protection block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmpIdentity {
    node_name: [u8; 64],
    device_name: [u8; 32],
}

impl MmpIdentity {
    /// Builds a fixed-width Linux MMP identity, truncating overlong names.
    pub fn from_names(node_name: &[u8], device_name: &[u8]) -> Self {
        let mut identity = Self::default();
        let node_len = core::cmp::min(node_name.len(), identity.node_name.len());
        let device_len = core::cmp::min(device_name.len(), identity.device_name.len());
        identity.node_name[..node_len].copy_from_slice(&node_name[..node_len]);
        identity.device_name[..device_len].copy_from_slice(&device_name[..device_len]);
        identity
    }

    pub(crate) const fn node_name(&self) -> &[u8; 64] {
        &self.node_name
    }

    pub(crate) const fn device_name(&self) -> &[u8; 32] {
        &self.device_name
    }
}

impl Default for MmpIdentity {
    fn default() -> Self {
        Self {
            node_name: [0; 64],
            device_name: [0; 32],
        }
    }
}

/// High-value, allocation-free events observable by an embedding runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Mount(MountEvent),
    Feature(FeatureEvent),
    Journal(JournalEvent),
    Recovery(RecoveryEvent),
    Integrity(IntegrityEvent),
    Repair(RepairEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountEvent {
    Started,
    Succeeded,
    Failed,
    UnmountStarted,
    Unmounted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureEvent {
    UnsupportedIncompat(u32),
    ReadOnlyCompat(u32),
    MissingCapability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalEvent {
    ReplayRequested,
    MetadataQueued,
    Committed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalReplayPhase {
    Initialize,
    Scan,
    Revoke,
    Replay,
    Persist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryEvent {
    Required,
    ReplayFailed {
        phase: JournalReplayPhase,
        cause: Ext4Error,
        persistence_error: Option<Ext4Error>,
    },
    JournalMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityEvent {
    ChecksumMismatch,
    CorruptionDetected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairEvent {
    RootRecreated,
    LostFoundRecreated,
    DirectoryIndexFallback,
}

pub trait Observer {
    fn event(&mut self, event: Event);
}

/// Observer used when the embedding runtime does not request events.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopObserver;

impl Observer for NoopObserver {
    fn event(&mut self, _event: Event) {}
}

/// Explicit bundle of small runtime capabilities.
///
/// This is a composition type, not a catch-all runtime trait. Algorithms take
/// the narrow capability they use, which keeps dependencies auditable.
pub struct MountServices<C, E, O, W = ()> {
    pub clock: C,
    pub entropy: E,
    pub observer: O,
    pub mmp_delay: W,
    pub mmp_identity: MmpIdentity,
}

/// Capabilities retained by a mounted filesystem after its clock has moved
/// into the private persistence owner.
///
/// The fields stay private so callers use typed filesystem operations instead
/// of reaching through the mount object to invoke providers directly.
pub struct MountedServices<E, O, W = ()> {
    pub(crate) entropy: E,
    pub(crate) observer: O,
    pub(crate) mmp_delay: W,
    pub(crate) mmp_identity: MmpIdentity,
}

impl<E, O, W> MountedServices<E, O, W> {
    pub(crate) const fn new(
        entropy: E,
        observer: O,
        mmp_delay: W,
        mmp_identity: MmpIdentity,
    ) -> Self {
        Self {
            entropy,
            observer,
            mmp_delay,
            mmp_identity,
        }
    }
}

impl<C, E, O> MountServices<C, E, O, ()> {
    pub const fn new(clock: C, entropy: E, observer: O) -> Self {
        Self {
            clock,
            entropy,
            observer,
            mmp_delay: (),
            mmp_identity: MmpIdentity {
                node_name: [0; 64],
                device_name: [0; 32],
            },
        }
    }
}

impl<C, E, O, W> MountServices<C, E, O, W> {
    /// Injects the mount-time MMP delay and diagnostic identity capabilities.
    pub fn with_mmp<N>(self, delay: N, identity: MmpIdentity) -> MountServices<C, E, O, N> {
        MountServices {
            clock: self.clock,
            entropy: self.entropy,
            observer: self.observer,
            mmp_delay: delay,
            mmp_identity: identity,
        }
    }
}
