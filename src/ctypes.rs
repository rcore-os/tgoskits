use core::{mem, ops::Not};

use arceos_posix_api as api;
use axerrno::LinuxError;
use bitflags::bitflags;

bitflags! {
    #[derive(Default, Debug)]
    pub struct SignalActionFlags: u32 {
        const SIGINFO = 4;
        const NODEFER = 0x40000000;
        const RESTORER = 0x04000000;
    }
}

/// Signal set. Corresponds to `struct sigset_t` in libc.
///
/// Currently we only support 32 standard signals.
#[derive(Default, Clone, Copy)]
#[repr(transparent)]
pub struct SignalSet {
    pub bits: [u32; 2],
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

#[derive(Clone, Copy)]
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct k_sigaction {
    handler: Option<unsafe extern "C" fn(i32)>,
    flags: u32,
    restorer: Option<unsafe extern "C" fn()>,
    mask: SignalSet,
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
    pub restorer: Option<unsafe extern "C" fn()>,
}
impl SignalAction {
    pub fn to_ctype(&self, dest: &mut k_sigaction) {
        dest.flags = self.flags.bits() as _;
        dest.mask = self.mask;
        match &self.disposition {
            SignalDisposition::Default => {
                dest.handler = None;
            }
            SignalDisposition::Ignore => {
                // SAFETY: SIG_IGN is 1
                dest.handler =
                    Some(unsafe { mem::transmute::<usize, unsafe extern "C" fn(i32)>(1) });
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
        Ok(SignalAction {
            flags,
            mask: value.mask,
            disposition,
            restorer: Some(
                // SAFETY: `axconfig::plat::SIGNAL_TRAMPOLINE` is a valid function pointer
                value.restorer.unwrap_or_else(|| unsafe {
                    mem::transmute(axconfig::plat::SIGNAL_TRAMPOLINE)
                }),
            ),
        })
    }
}

/// Signal information. Corresponds to `struct siginfo_t` in libc.
#[derive(Default, Clone)]
#[repr(transparent)]
pub struct SignalInfo(pub api::ctypes::siginfo_t);
impl SignalInfo {
    pub const SI_USER: u32 = 0;

    pub fn new(signo: u32, code: u32) -> Self {
        Self(api::ctypes::siginfo_t {
            si_signo: signo as _,
            si_errno: 0,
            si_code: code as _,
            ..Default::default()
        })
    }

    pub fn signo(&self) -> u32 {
        self.0.si_signo as u32
    }
}
