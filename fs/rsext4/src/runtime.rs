//! OS-independent runtime capabilities used by the ext4 core.

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

/// Encryption algorithms that can be requested by ext4 policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionAlgorithm {
    Aes256Xts,
    Aes256Cts,
    Adiantum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestAlgorithm {
    Sha256,
    Sha512,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoOperation {
    Encrypt,
    Decrypt,
}

/// A key lookup descriptor stored as pure filesystem policy data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyDescriptor<'a> {
    pub identifier: &'a [u8],
    pub purpose: KeyPurpose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPurpose {
    FileContents,
    FileNames,
    VeritySignature,
}

/// Synchronous cryptographic primitives required by filesystem policies.
pub trait CryptoProvider {
    fn crypt(
        &mut self,
        operation: CryptoOperation,
        algorithm: EncryptionAlgorithm,
        key: &[u8],
        nonce: &[u8],
        input: &[u8],
        output: &mut [u8],
    ) -> Ext4Result<()>;

    fn digest(
        &mut self,
        algorithm: DigestAlgorithm,
        input: &[u8],
        output: &mut [u8],
    ) -> Ext4Result<usize>;
}

/// Key retrieval capability. Keyring ownership and policy stay in the OS.
pub trait KeyProvider {
    fn read_key(&mut self, descriptor: KeyDescriptor<'_>, output: &mut [u8]) -> Ext4Result<usize>;
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
pub struct MountServices<C, E, P, K, O> {
    pub clock: C,
    pub entropy: E,
    pub crypto: P,
    pub keys: K,
    pub observer: O,
}

/// Capabilities retained by a mounted filesystem after its clock has moved
/// into the private persistence owner.
///
/// The fields stay private so callers use typed filesystem operations instead
/// of reaching through the mount object to invoke providers directly.
pub struct MountedServices<E, P, K, O> {
    pub(crate) _entropy: E,
    pub(crate) _crypto: P,
    pub(crate) _keys: K,
    pub(crate) observer: O,
}

impl<E, P, K, O> MountedServices<E, P, K, O> {
    pub(crate) const fn new(entropy: E, crypto: P, keys: K, observer: O) -> Self {
        Self {
            _entropy: entropy,
            _crypto: crypto,
            _keys: keys,
            observer,
        }
    }
}

impl<C, E, P, K, O> MountServices<C, E, P, K, O> {
    pub const fn new(clock: C, entropy: E, crypto: P, keys: K, observer: O) -> Self {
        Self {
            clock,
            entropy,
            crypto,
            keys,
            observer,
        }
    }
}
