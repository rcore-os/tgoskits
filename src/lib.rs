#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;

pub mod ctypes;

use core::alloc::Layout;

use axhal::arch::TrapFrame;
use ctypes::{SignalAction, SignalActionFlags, SignalDisposition, SignalInfo, SignalSet};

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

/// Signal action that should be properly handled by the OS.
///
/// See [`SignalManager::check_signals`] for details.
pub enum SignalOSAction {
    /// Terminate the process.
    Terminate,
    /// Generate a core dump and terminate the process.
    CoreDump,
    /// Stop the process.
    Stop,
    /// Continue the process if stopped.
    Continue,
    /// A handler is pushed into the signal stack. The OS should add the
    /// corresponding signals to the blocked set.
    Handler { add_blocked: SignalSet },
}

/// Structure to record pending signals.
pub struct PendingSignals {
    /// The pending signals.
    ///
    /// Note that does not correspond to `pending signals` as described in
    /// Linux. `Pending signals` in Linux refers to the signals that are
    /// delivered but blocked from delivery, while `pending` here refers to any
    /// signal that is delivered and not yet handled.
    pub pending: SignalSet,
    pending_info: [Option<SignalInfo>; 32],
}
impl PendingSignals {
    pub fn new() -> Self {
        Self {
            pending: SignalSet::default(),
            pending_info: Default::default(),
        }
    }

    pub fn send_signal(&mut self, sig: SignalInfo) -> bool {
        let signo = sig.signo();
        if !self.pending.add(signo) {
            return false;
        }
        self.pending_info[signo as usize] = Some(sig);
        true
    }

    /// Dequeue the next pending signal contained in `mask`, if any.
    pub fn dequeue_signal(&mut self, mask: &SignalSet) -> Option<SignalInfo> {
        self.pending
            .dequeue(mask)
            .and_then(|signo| self.pending_info[signo as usize].take())
    }
}

pub struct SignalFrame {
    tf: TrapFrame,
    blocked: SignalSet,
    siginfo: SignalInfo,
}

/// Handle a signal.
///
/// Returns `Some(action)` if the signal is not ignored. In such case, the
/// OS should execute the action accordingly (or do nothing if the action is
/// [`SignalOSAction::Nothing`]).
pub fn handle_signal(
    tf: &mut TrapFrame,
    restore_blocked: SignalSet,
    sig: SignalInfo,
    action: &SignalAction,
) -> Option<SignalOSAction> {
    let signo = sig.signo();
    info!("Handle signal: {}", signo);
    match action.disposition {
        SignalDisposition::Default => match DEFAULT_ACTIONS[signo as usize] {
            DefaultSignalAction::Terminate => Some(SignalOSAction::Terminate),
            DefaultSignalAction::CoreDump => Some(SignalOSAction::CoreDump),
            DefaultSignalAction::Stop => Some(SignalOSAction::Stop),
            DefaultSignalAction::Ignore => None,
            DefaultSignalAction::Continue => Some(SignalOSAction::Continue),
        },
        SignalDisposition::Ignore => None,
        SignalDisposition::Handler(handler) => {
            let layout = Layout::new::<SignalFrame>();
            let aligned_sp = (tf.sp() - layout.size()) & !(layout.align() - 1);

            let frame_ptr = aligned_sp as *mut SignalFrame;
            // SAFETY: pointer is valid
            let frame = unsafe { &mut *frame_ptr };

            *frame = SignalFrame {
                tf: *tf,
                blocked: restore_blocked,
                siginfo: sig,
            };

            tf.set_ip(handler as usize);
            tf.set_sp(aligned_sp);
            tf.set_arg0(signo as _);
            tf.set_arg1(&frame.siginfo as *const _ as _);
            tf.set_arg2(frame_ptr as _);

            let restorer = action.restorer.map_or(0, |f| f as _);
            #[cfg(target_arch = "x86_64")]
            tf.push_ra(restorer);
            #[cfg(not(target_arch = "x86_64"))]
            tf.set_ra(restorer);

            let mut add_blocked = action.mask;
            if !action.flags.contains(SignalActionFlags::NODEFER) {
                add_blocked.add(signo);
            }
            Some(SignalOSAction::Handler { add_blocked })
        }
    }
}

/// Restore the signal frame. Called by `sigreutrn`.
pub fn restore(tf: &mut TrapFrame, blocked: &mut SignalSet) {
    let frame_ptr = tf.sp() as *const SignalFrame;
    // SAFETY: pointer is valid
    let frame = unsafe { &*frame_ptr };

    *tf = frame.tf;

    *blocked = frame.blocked;
}
