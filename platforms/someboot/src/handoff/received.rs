//! Per-image immutable metadata, independent of the producer's EFI table.
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use super::{ApplicationMetadata, MAX_METADATA};

struct Metadata {
    id: [u8; 16],
    version: u32,
    len: usize,
    bytes: [u8; MAX_METADATA],
}
struct Storage(UnsafeCell<Metadata>);
// SAFETY: the boot CPU writes once before SMP, then readers see immutable data.
unsafe impl Sync for Storage {}
static STORAGE: Storage = Storage(UnsafeCell::new(Metadata {
    id: [0; 16],
    version: 0,
    len: 0,
    bytes: [0; MAX_METADATA],
}));
static READY: AtomicBool = AtomicBool::new(false);

/// # Safety
/// Boot CPU only, once per image, before any metadata consumer or SMP.
pub(crate) unsafe fn remember(metadata: Option<ApplicationMetadata<'_>>) {
    let Some(metadata) = metadata else {
        return;
    };
    metadata
        .validate()
        .expect("invalid inherited application metadata");
    // SAFETY: the boot caller excludes readers and all other writers.
    let storage = unsafe { &mut *STORAGE.0.get() };
    storage.id = metadata.id;
    storage.version = metadata.version;
    storage.len = metadata.bytes.len();
    storage.bytes[..storage.len].copy_from_slice(metadata.bytes);
    READY.store(true, Ordering::Release);
}
pub(crate) fn metadata() -> Option<ApplicationMetadata<'static>> {
    if !READY.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: Acquire observes the one-time write; bytes are immutable afterward.
    let storage = unsafe { &*STORAGE.0.get() };
    Some(ApplicationMetadata {
        id: storage.id,
        version: storage.version,
        bytes: &storage.bytes[..storage.len],
    })
}
