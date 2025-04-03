#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;

pub mod ctypes;

use core::alloc::Layout;

use alloc::sync::Arc;
use axhal::arch::TrapFrame;
use axptr::{AddrSpaceProvider, UserConstPtr};
use axtask::WaitQueue;
use ctypes::{SignalAction, SignalActionFlags, SignalDisposition, SignalInfo, SignalSet};

pub const SIGKILL: u32 = 9;
pub const SIGSTOP: u32 = 19;

#[derive(Debug)]
enum DefaultSignalAction {
    /// Terminate the process.
    Terminate,

    /// Ignore the signal.
    Ignore,

    /// Terminate the process and generate a core dump.
    CoreDump,

    /// Stop the process.
    Stop,

    /// Continue the process if stopped.
    Continue,
}
const DEFAULT_ACTIONS: [DefaultSignalAction; 32] = [
    // Unspecified
    DefaultSignalAction::Ignore,
    // SIGHUP
    DefaultSignalAction::Terminate,
    // SIGINT
    DefaultSignalAction::Terminate,
    // SIGQUIT
    DefaultSignalAction::CoreDump,
    // SIGILL
    DefaultSignalAction::CoreDump,
    // SIGTRAP
    DefaultSignalAction::CoreDump,
    // SIGABRT
    DefaultSignalAction::CoreDump,
    // SIGBUS
    DefaultSignalAction::CoreDump,
    // SIGFPE
    DefaultSignalAction::CoreDump,
    // SIGKILL
    DefaultSignalAction::Terminate,
    // SIGUSR1
    DefaultSignalAction::Terminate,
    // SIGSEGV
    DefaultSignalAction::CoreDump,
    // SIGUSR2
    DefaultSignalAction::Terminate,
    // SIGPIPE
    DefaultSignalAction::Terminate,
    // SIGALRM
    DefaultSignalAction::Terminate,
    // SIGTERM
    DefaultSignalAction::Terminate,
    // SIGSTKFLT
    DefaultSignalAction::Terminate,
    // SIGCHLD
    DefaultSignalAction::Ignore,
    // SIGCONT
    DefaultSignalAction::Continue,
    // SIGSTOP
    DefaultSignalAction::Stop,
    // SIGTSTP
    DefaultSignalAction::Stop,
    // SIGTTIN
    DefaultSignalAction::Stop,
    // SIGTTOU
    DefaultSignalAction::Stop,
    // SIGURG
    DefaultSignalAction::Ignore,
    // SIGXCPU
    DefaultSignalAction::CoreDump,
    // SIGXFSZ
    DefaultSignalAction::CoreDump,
    // SIGVTALRM
    DefaultSignalAction::Terminate,
    // SIGPROF
    DefaultSignalAction::Terminate,
    // SIGWINCH
    DefaultSignalAction::Ignore,
    // SIGIO
    DefaultSignalAction::Terminate,
    // SIGPWR
    DefaultSignalAction::Terminate,
    // SIGSYS
    DefaultSignalAction::CoreDump,
];

/// Structure to manage signal handling in a task.
pub struct SignalManager {
    /// The set of signals currently blocked from delivery.
    pub blocked: SignalSet,

    /// The signal actions.
    actions: [SignalAction; 32],

    /// The pending signals.
    ///
    /// Note that does not correspond to `pending signals` as described in
    /// Linux. `Pending signals` in Linux refers to the signals that are
    /// delivered but blocked from delivery, while `pending` here refers to any
    /// signal that is delivered and not yet handled.
    pub pending: SignalSet,
    pending_info: [Option<SignalInfo>; 32],

    /// The signals the task is currently waiting for. Used by
    /// `sys_rt_sigtimedwait`.
    pub waiting: SignalSet,
    pub wq: Arc<WaitQueue>,

    /// If this is true, no more custom signal handler function should be
    /// written to the trap frame, until it is set back to false at the end of
    /// [`post_trap_callback`].
    pub prevent_signal_handling: bool,
}
impl SignalManager {
    pub fn new() -> Self {
        Self {
            blocked: SignalSet::default(),
            actions: Default::default(),

            pending: SignalSet::default(),
            pending_info: Default::default(),

            waiting: SignalSet::default(),
            wq: Arc::new(WaitQueue::new()),

            prevent_signal_handling: false,
        }
    }

    pub fn send_signal(&mut self, sig: SignalInfo) -> bool {
        let signo = sig.signo();
        if !self.pending.add(signo) {
            return false;
        }
        self.pending_info[signo as usize] = Some(sig);
        if self.waiting.has(signo) {
            self.wq.notify_one(false);
        }
        true
    }

    pub fn set_action(&mut self, signo: u32, action: SignalAction) {
        self.actions[signo as usize] = action;
    }
    pub fn action(&self, signo: u32) -> &SignalAction {
        &self.actions[signo as usize]
    }

    /// Dequeue the next pending signal contained in `mask`, if any.
    pub fn dequeue_signal_in(&mut self, mask: &SignalSet) -> Option<SignalInfo> {
        self.pending
            .dequeue(mask)
            .and_then(|signo| self.pending_info[signo as usize].take())
    }

    /// Dequeue the next non-blocked pending signal.
    pub fn dequeue_signal(&mut self) -> Option<SignalInfo> {
        self.dequeue_signal_in(&!self.blocked)
    }

    /// Run the signal handler for the given signal.
    ///
    /// Returns `true` if the process should be terminated or a signal handler
    /// should be executed.
    pub fn run_action(&mut self, tf: &mut TrapFrame, sig: SignalInfo) -> bool {
        let signo = sig.signo();
        info!("Handle signal: {}", signo);
        let action = &self.actions[signo as usize];
        match action.disposition {
            SignalDisposition::Default => match DEFAULT_ACTIONS[signo as usize] {
                DefaultSignalAction::Terminate => axtask::exit(128 + signo as i32),
                DefaultSignalAction::CoreDump => {
                    warn!("Core dump not implemented");
                    axtask::exit(128 + signo as i32);
                }
                DefaultSignalAction::Stop => {
                    warn!("Stop not implemented");
                    axtask::exit(-1);
                }
                DefaultSignalAction::Ignore => false,
                DefaultSignalAction::Continue => {
                    warn!("Continue not implemented");
                    true
                }
            },
            SignalDisposition::Ignore => false,
            SignalDisposition::Handler(handler) => {
                let layout = Layout::new::<SignalFrame>();
                let aligned_sp = (tf.sp() - layout.size()) & !(layout.align() - 1);

                let frame_ptr = aligned_sp as *mut SignalFrame;
                // SAFETY: pointer is valid
                let frame = unsafe { &mut *frame_ptr };

                *frame = SignalFrame {
                    tf: *tf,
                    blocked: self.blocked,
                    siginfo: sig,
                };

                tf.set_ip(handler as _);
                tf.set_sp(aligned_sp);
                tf.set_arg0(signo as _);
                tf.set_arg1(&frame.siginfo as *const _ as _);
                tf.set_arg2(frame_ptr as _);

                let restorer = action.restorer.map_or(0, |f| f as _);
                #[cfg(target_arch = "x86_64")]
                tf.push_ra(restorer);
                #[cfg(not(target_arch = "x86_64"))]
                tf.set_ra(restorer);

                let mut mask = action.mask;
                if !action.flags.contains(SignalActionFlags::NODEFER) {
                    mask.add(signo);
                }
                self.blocked.add_from(&mask);
                true
            }
        }
    }

    /// Check and handle pending signals.
    ///
    /// Should be called in the post trap callback.
    pub fn check_signals(&mut self, tf: &mut TrapFrame) {
        if self.prevent_signal_handling {
            self.prevent_signal_handling = false;
            return;
        }
        while let Some(sig) = self.dequeue_signal() {
            if self.run_action(tf, sig) {
                break;
            }
        }
    }

    /// Restore the signal frame. Called by `sigreutrn`.
    pub fn restore(&mut self, tf: &mut TrapFrame, aspace: impl AddrSpaceProvider) {
        let frame_ptr: UserConstPtr<SignalFrame> = tf.sp().into();
        let frame = frame_ptr.get(aspace).expect("invalid frame ptr");

        *tf = frame.tf;
        #[cfg(any(
            target_arch = "riscv32",
            target_arch = "riscv64",
            target_arch = "loongarch64"
        ))]
        tf.set_ip(tf.ip() - 4);

        self.blocked = frame.blocked;
    }
}

pub struct SignalFrame {
    tf: TrapFrame,
    blocked: SignalSet,
    siginfo: SignalInfo,
}
