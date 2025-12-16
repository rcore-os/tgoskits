use alloc::{borrow::Cow, sync::Arc};
use core::{
    any::Any,
    mem,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use axerrno::{AxError, AxResult};
use axio::{BufMut, Write};
use axpoll::{IoEvents, PollSet, Pollable};
use axtask::{
    current,
    future::{block_on, poll_io},
};
use spin::RwLock;
use starry_core::task::AsThread;
use starry_signal::{SignalInfo, SignalSet};
use zerocopy::{Immutable, IntoBytes};

use crate::file::{FileLike, Kstat, SealedBufMut};

/// The size of signalfd_siginfo structure (128 bytes as per Linux
/// specification)
const SIGNALFD_SIGINFO_SIZE: usize = 128;

/// signalfd_siginfo structure layout
/// This matches the Linux signalfd_siginfo structure (128 bytes)
#[repr(C)]
#[derive(Immutable, IntoBytes)]
struct SignalfdSiginfo {
    ssi_signo: u32,    // Signal number
    ssi_errno: i32,    // Error number (unused)
    ssi_code: i32,     // Signal code
    ssi_pid: u32,      // PID of sender
    ssi_uid: u32,      // Real UID of sender
    ssi_fd: i32,       // File descriptor (SIGIO)
    ssi_tid: u32,      // Kernel timer ID (POSIX timers)
    ssi_band: u32,     // Band event (SIGIO)
    ssi_overrun: u32,  // POSIX timer overrun count
    ssi_trapno: u32,   // Trap number that caused signal
    ssi_status: i32,   // Exit status or signal (SIGCHLD)
    ssi_int: i32,      // Integer sent by sigqueue(2)
    ssi_ptr: u64,      // Pointer sent by sigqueue(2)
    ssi_utime: u64,    // User CPU time consumed (SIGCHLD)
    ssi_stime: u64,    // System CPU time consumed (SIGCHLD)
    ssi_addr: u64,     // Address that generated signal
    ssi_addr_lsb: u16, // Least significant bit of address
    _pad: [u8; 46],    // Padding to make it 128 bytes
}

const _: [(); SIGNALFD_SIGINFO_SIZE] = [(); mem::size_of::<SignalfdSiginfo>()];

impl SignalfdSiginfo {
    /// Convert from SignalInfo to signalfd_siginfo
    fn from_signal_info(sig_info: &SignalInfo) -> Self {
        let errno = sig_info.errno();

        SignalfdSiginfo {
            ssi_signo: sig_info.signo() as u32,
            ssi_errno: errno,
            ssi_code: sig_info.code(),
            ssi_pid: 0,
            ssi_uid: 0,
            ssi_fd: -1,
            ssi_tid: 0,
            ssi_band: 0,
            ssi_overrun: 0,
            ssi_trapno: 0,
            ssi_status: 0,
            ssi_int: 0,
            ssi_ptr: 0,
            ssi_utime: 0,
            ssi_stime: 0,
            ssi_addr: 0,
            ssi_addr_lsb: 0,
            _pad: [0u8; 46],
        }
    }
}

pub struct Signalfd {
    mask: RwLock<SignalSet>,
    non_blocking: AtomicBool,
    poll_rx: PollSet,
}

impl Signalfd {
    pub fn new(mask: SignalSet) -> Arc<Self> {
        Arc::new(Self {
            mask: RwLock::new(mask),
            non_blocking: AtomicBool::new(false),
            poll_rx: PollSet::new(),
        })
    }

    pub fn update_mask(&self, mask: SignalSet) {
        *self.mask.write() = mask;
        self.poll_rx.wake();
    }

    fn mask(&self) -> SignalSet {
        *self.mask.read()
    }

    /// Check if there are any pending signals matching the mask
    fn has_pending_signals(&self) -> bool {
        let mask = self.mask();
        let curr = current();
        let signal = &curr.as_thread().signal;
        let pending = signal.pending();
        !(pending & mask).is_empty()
    }

    /// Dequeue a signal matching the mask
    fn dequeue_signal(&self) -> Option<SignalInfo> {
        let mask = self.mask();
        let curr = current();
        let signal = &curr.as_thread().signal;
        signal.dequeue_signal(&mask)
    }
}

impl FileLike for Signalfd {
    fn read(&self, dst: &mut SealedBufMut) -> AxResult<usize> {
        if dst.remaining_mut() < SIGNALFD_SIGINFO_SIZE {
            return Err(AxError::InvalidInput);
        }

        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            if let Some(sig_info) = self.dequeue_signal() {
                // Convert SignalInfo to SignalfdSiginfo
                let sfd_info = SignalfdSiginfo::from_signal_info(&sig_info);

                // Write the structure to the destination buffer
                let bytes = sfd_info.as_bytes();
                dst.write(bytes)?;

                // Wake up other waiters if there are more signals pending
                if self.has_pending_signals() {
                    self.poll_rx.wake();
                }

                Ok(SIGNALFD_SIGINFO_SIZE)
            } else {
                Err(AxError::WouldBlock)
            }
        }))
    }

    fn write(&self, _src: &mut crate::file::SealedBuf) -> AxResult<usize> {
        // signalfd is read-only
        Err(AxError::BadFileDescriptor)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, non_blocking: bool) -> AxResult {
        self.non_blocking.store(non_blocking, Ordering::Release);
        Ok(())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[signalfd]".into()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl Pollable for Signalfd {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, self.has_pending_signals());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_rx.register(context.waker());
        }
    }
}
