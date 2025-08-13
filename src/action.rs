use core::ffi::c_ulong;

use bitflags::bitflags;
use linux_raw_sys::{
    general::{
        __kernel_sighandler_t, __sigrestore_t, SA_NODEFER, SA_ONSTACK, SA_RESETHAND, SA_RESTART,
        SA_SIGINFO, kernel_sigaction,
    },
    signal_macros::sig_ign,
};

use crate::SignalSet;

#[derive(Debug)]
pub enum DefaultSignalAction {
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

/// Signal action that should be properly handled by the OS.
///
/// See [`SignalManager::check_signals`] for details.
#[derive(Debug)]
pub enum SignalOSAction {
    /// Terminate the process.
    Terminate,
    /// Generate a core dump and terminate the process.
    CoreDump,
    /// Stop the process.
    Stop,
    /// Continue the process if stopped.
    Continue,
    /// A signal handler is pushed into the signal stack. The OS doesn't need to
    /// do anything.
    Handler,
}

bitflags! {
    #[derive(Default, Debug, Clone, Copy)]
    pub struct SignalActionFlags: c_ulong {
        const SIGINFO = SA_SIGINFO as _;
        const NODEFER = SA_NODEFER as _;
        const RESETHAND = SA_RESETHAND as _;
        const RESTART = SA_RESTART as _;
        const ONSTACK = SA_ONSTACK as _;
        const RESTORER = 0x4000000;
    }
}

// FIXME: replace with `kernel_sigaction` after finishing above "TODO"s for `SignalSet`
#[derive(Debug, Clone, Copy)]
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct k_sigaction {
    handler: __kernel_sighandler_t,
    flags: c_ulong,
    restorer: __sigrestore_t,
    pub mask: SignalSet,
}

#[derive(Debug, Default, Clone)]
pub enum SignalDisposition {
    #[default]
    /// Use the default signal action.
    Default,
    /// Ignore the signal.
    Ignore,
    /// Custom signal handler.
    Handler(unsafe extern "C" fn(i32)),
}

/// Signal action. Corresponds to `struct sigaction` in libc.
#[derive(Debug, Clone, Default)]
pub struct SignalAction {
    pub flags: SignalActionFlags,
    pub mask: SignalSet,
    pub disposition: SignalDisposition,
    pub restorer: __sigrestore_t,
}

impl From<SignalAction> for kernel_sigaction {
    fn from(value: SignalAction) -> Self {
        // FIXME: Zeroable
        let mut result: kernel_sigaction = unsafe { core::mem::zeroed() };

        result.sa_flags = value.flags.bits() as _;
        result.sa_mask = value.mask.into();
        match &value.disposition {
            SignalDisposition::Default => {
                result.sa_handler_kernel = None;
            }
            SignalDisposition::Ignore => {
                result.sa_handler_kernel = sig_ign();
            }
            SignalDisposition::Handler(handler) => {
                result.sa_handler_kernel = Some(*handler);
            }
        }
        #[cfg(sa_restorer)]
        {
            result.sa_restorer = self.restorer;
        }

        result
    }
}

impl From<kernel_sigaction> for SignalAction {
    fn from(value: kernel_sigaction) -> Self {
        let flags = SignalActionFlags::from_bits_truncate(value.sa_flags);
        let disposition = {
            match value.sa_handler_kernel {
                None => {
                    // SIG_DFL
                    SignalDisposition::Default
                }
                Some(h) if h as usize == 1 => {
                    // SIG_IGN
                    SignalDisposition::Ignore
                }
                Some(h) => {
                    // Custom signal handler
                    SignalDisposition::Handler(h)
                }
            }
        };

        #[cfg(sa_restorer)]
        let restorer = if flags.contains(SignalActionFlags::RESTORER) {
            value.sa_restorer
        } else {
            None
        };
        #[cfg(not(sa_restorer))]
        let restorer = None;

        SignalAction {
            flags,
            mask: value.sa_mask.into(),
            disposition,
            restorer,
        }
    }
}
