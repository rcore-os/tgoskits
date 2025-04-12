use core::{
    ffi::c_ulong,
    mem,
    ops::{BitOr, Not},
};

use axerrno::LinuxError;
use bitflags::bitflags;
use linux_raw_sys::{
    general::{
        __kernel_sighandler_t, __sigrestore_t, SA_NODEFER, SA_RESETHAND, SA_RESTART, SA_SIGINFO,
        siginfo_t,
    },
    signal_macros::{SIG_DFL, sig_ign},
};

bitflags! {
    #[derive(Default, Debug)]
    pub struct SignalActionFlags: c_ulong {
        const SIGINFO = SA_SIGINFO as _;
        const NODEFER = SA_NODEFER as _;
        const RESETHAND = SA_RESETHAND as _;
        const RESTART = SA_RESTART as _;
        const RESTORER = 0x4000000;
    }
}

/// Signal set. Corresponds to `struct sigset_t` in libc.
///
/// Currently we only support 32 standard signals.
// TODO: wrap around `kernel_sigset_t`
// TODO: real-time signals
#[derive(Default, Debug, Clone, Copy)]
#[repr(transparent)]
pub struct SignalSet {
    bits: [u32; 2],
}
impl SignalSet {
    pub fn add(&mut self, signal: u32) -> bool {
        if !(1..32).contains(&signal) {
            return false;
        }
        let bit = 1 << (signal - 1);
        if self.bits[0] & bit != 0 {
            return false;
        }
        self.bits[0] |= bit;
        true
    }
    pub fn remove(&mut self, signal: u32) -> bool {
        if !(1..32).contains(&signal) {
            return false;
        }
        let bit = 1 << (signal - 1);
        if self.bits[0] & bit == 0 {
            return false;
        }
        self.bits[0] &= !bit;
        true
    }

    pub fn has(&self, signal: u32) -> bool {
        (1..32).contains(&signal) && (self.bits[0] & (1 << (signal - 1))) != 0
    }

    pub fn add_from(&mut self, other: &SignalSet) {
        self.bits[0] |= other.bits[0];
        self.bits[1] |= other.bits[1];
    }
    pub fn remove_from(&mut self, other: &SignalSet) {
        self.bits[0] &= !other.bits[0];
        self.bits[1] &= !other.bits[1];
    }

    /// Dequeue the a signal in `mask` from this set, if any.
    pub fn dequeue(&mut self, mask: &SignalSet) -> Option<u32> {
        let bits = self.bits[0] & mask.bits[0];
        if bits == 0 {
            None
        } else {
            let signal = bits.trailing_zeros();
            self.bits[0] &= !(1 << signal);
            Some(signal + 1)
        }
    }
}

impl Not for SignalSet {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self {
            bits: [!self.bits[0], !self.bits[1]],
        }
    }
}
impl BitOr for SignalSet {
    type Output = Self;

    fn bitor(self, other: Self) -> Self::Output {
        Self {
            bits: [self.bits[0] | other.bits[0], self.bits[1] | other.bits[1]],
        }
    }
}

// FIXME: replace with `kernel_sigaction` after finishing above "TODO"s for `SignalSet`
#[derive(Clone, Copy)]
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct k_sigaction {
    handler: __kernel_sighandler_t,
    flags: c_ulong,
    restorer: __sigrestore_t,
    pub mask: SignalSet,
}

#[derive(Default)]
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
#[derive(Default)]
pub struct SignalAction {
    pub flags: SignalActionFlags,
    pub mask: SignalSet,
    pub disposition: SignalDisposition,
    pub restorer: __sigrestore_t,
}
impl SignalAction {
    pub fn to_ctype(&self, dest: &mut k_sigaction) {
        dest.flags = self.flags.bits() as _;
        dest.mask = self.mask;
        match &self.disposition {
            SignalDisposition::Default => {
                dest.handler = SIG_DFL;
            }
            SignalDisposition::Ignore => {
                dest.handler = sig_ign();
            }
            SignalDisposition::Handler(handler) => {
                dest.handler = Some(*handler);
            }
        }
        dest.restorer = self.restorer;
    }
}

impl TryFrom<k_sigaction> for SignalAction {
    type Error = LinuxError;

    fn try_from(value: k_sigaction) -> Result<Self, Self::Error> {
        let Some(flags) = SignalActionFlags::from_bits(value.flags) else {
            warn!("unrecognized signal flags: {}", value.flags);
            return Err(LinuxError::EINVAL);
        };
        let disposition = {
            match value.handler {
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

        // SAFETY: `axconfig::plat::SIGNAL_TRAMPOLINE` is a valid function pointer
        let default_restorer: __sigrestore_t =
            unsafe { mem::transmute(axconfig::plat::SIGNAL_TRAMPOLINE) };

        // #[cfg(sa_restorer)]
        let restorer = if flags.contains(SignalActionFlags::RESTORER) {
            value.restorer.or(default_restorer)
        } else {
            default_restorer
        };
        // #[cfg(not(sa_restorer))]
        // let restorer = default_restorer;

        Ok(SignalAction {
            flags,
            mask: value.mask,
            disposition,
            restorer,
        })
    }
}

/// Signal information. Corresponds to `struct siginfo_t` in libc.
#[derive(Clone)]
pub struct SignalInfo {
    signo: u32,
    code: u32,
}

impl SignalInfo {
    pub fn new(signo: u32, code: u32) -> Self {
        Self { signo, code }
    }

    pub fn to_ctype(&self, dest: &mut siginfo_t) {
        dest.__bindgen_anon_1.__bindgen_anon_1.si_signo = self.signo as _;
        dest.__bindgen_anon_1.__bindgen_anon_1.si_code = self.code as _;
    }

    pub fn signo(&self) -> u32 {
        self.signo
    }

    pub fn code(&self) -> u32 {
        self.code
    }
}
