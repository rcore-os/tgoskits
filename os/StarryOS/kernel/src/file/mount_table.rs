use alloc::{
    borrow::Cow,
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
};
use core::{
    sync::atomic::{AtomicU64, Ordering},
    task::Context,
};

use ax_errno::AxResult;
use ax_fs_ng::vfs::{FileBackend, FileFlags, MountNamespace};
use ax_kspin::SpinNoIrq;
use axpoll::{IoEvents, PollSet, Pollable};
use spin::Once;

use super::{File, FileLike, IoDst, IoSrc, Kstat};

const MOUNT_CHANGE_EVENTS: IoEvents = IoEvents::PRI.union(IoEvents::ERR);

static MOUNT_NAMESPACE_EVENTS: Once<SpinNoIrq<BTreeMap<u64, Weak<MountNamespaceEvent>>>> =
    Once::new();

fn event_registry() -> &'static SpinNoIrq<BTreeMap<u64, Weak<MountNamespaceEvent>>> {
    MOUNT_NAMESPACE_EVENTS.call_once(|| SpinNoIrq::new(BTreeMap::new()))
}

fn event_for_open(namespace: &MountNamespace) -> Arc<MountNamespaceEvent> {
    let mut registry = event_registry().lock();
    registry.retain(|_, event| event.strong_count() != 0);
    if let Some(event) = registry.get(&namespace.id()).and_then(Weak::upgrade) {
        return event;
    }

    let event = Arc::new(MountNamespaceEvent::new());
    registry.insert(namespace.id(), Arc::downgrade(&event));
    event
}

/// Reports a mount table change to files opened in `namespace`.
pub(crate) fn notify_mount_namespace_changed(namespace: &MountNamespace) {
    let event = event_registry()
        .lock()
        .get(&namespace.id())
        .and_then(Weak::upgrade);
    if let Some(event) = event {
        event.notify();
    }
}

struct MountNamespaceEvent {
    generation: AtomicU64,
    waiters: PollSet,
}

impl MountNamespaceEvent {
    const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            waiters: PollSet::new(),
        }
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn notify(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        // SAFETY: mount syscalls run in task context after releasing filesystem
        // and namespace locks. The generation is published before waking.
        unsafe {
            self.waiters.wake(MOUNT_CHANGE_EVENTS);
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        let interests = events & MOUNT_CHANGE_EVENTS;
        if interests.is_empty() {
            return;
        }
        // SAFETY: poll registration runs in task context without mount or
        // registry locks held.
        unsafe {
            self.waiters.register(context.waker(), interests);
        }
    }
}

/// One open file description for `/proc/.../{mountinfo,mounts}`.
pub(crate) struct MountTableFile {
    file: Arc<File>,
    event: Arc<MountNamespaceEvent>,
    observed_generation: AtomicU64,
}

impl MountTableFile {
    pub(crate) fn new(file: Arc<File>, namespace: &MountNamespace) -> Arc<Self> {
        let event = event_for_open(namespace);
        let observed_generation = AtomicU64::new(event.generation());
        Arc::new(Self {
            file,
            event,
            observed_generation,
        })
    }

    pub(crate) fn inner(&self) -> &Arc<File> {
        &self.file
    }
}

impl FileLike for MountTableFile {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        self.file.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        self.file.write(src)
    }

    fn stat(&self) -> AxResult<Kstat> {
        self.file.stat()
    }

    fn inode_key(&self) -> Option<(u64, u64)> {
        self.file.inode_key()
    }

    fn file_mmap(&self) -> AxResult<(FileBackend, FileFlags)> {
        self.file.file_mmap()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        self.file.ioctl(cmd, arg)
    }

    fn open_flags(&self) -> u32 {
        self.file.open_flags()
    }

    fn nonblocking(&self) -> bool {
        self.file.nonblocking()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> AxResult {
        self.file.set_nonblocking(nonblocking)
    }

    fn append(&self) -> bool {
        self.file.append()
    }

    fn set_append(&self, append: bool) -> AxResult {
        self.file.set_append(append)
    }

    fn path(&self) -> Cow<'_, str> {
        self.file.path()
    }
}

impl Pollable for MountTableFile {
    fn poll(&self) -> IoEvents {
        let generation = self.event.generation();
        let observed = self.observed_generation.swap(generation, Ordering::AcqRel);
        let changed = if generation != observed {
            MOUNT_CHANGE_EVENTS
        } else {
            IoEvents::empty()
        };
        self.file.poll() | changed
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.event.register(context, events);
        self.file.register(context, events);
    }
}
