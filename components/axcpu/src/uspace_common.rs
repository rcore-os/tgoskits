use core::marker::PhantomData;

use ax_memory_addr::VirtAddr;

use crate::{trap::PageFaultFlags, uspace::ExceptionInfo};

/// An architecture interrupt token captured while returning from user mode.
///
/// The token remains undispatched so the runtime can first publish the
/// user-to-kernel accounting transition. Its numeric representation is an
/// architecture-owned trap vector or cause and has no meaning outside the
/// runtime IRQ dispatcher.
#[must_use = "a raw user interrupt must be dispatched by the runtime"]
#[derive(Debug)]
#[repr(transparent)]
pub struct RawUserInterrupt {
    raw: usize,
    not_send: PhantomData<*mut ()>,
}

impl RawUserInterrupt {
    /// Creates an undispatched interrupt token at the architecture boundary.
    pub(crate) const fn new(raw: usize) -> Self {
        Self {
            raw,
            not_send: PhantomData,
        }
    }

    /// Consumes and dispatches the interrupt through the registered trap hook.
    ///
    /// User entry returned with raw IRQs masked and this linear token owns the
    /// continuation that will eventually restore the user interrupt state.
    pub fn dispatch(self) -> bool {
        // SAFETY: `RawUserInterrupt` is created only while decoding the unique
        // raw user exception token. Consuming it preserves that ownership.
        let permit = unsafe { crate::trap::TrapIrqPermit::from_arch_entry(self.raw) };
        crate::trap::dispatch_irq(permit)
    }
}

/// An opaque token proving that one architecture user entry has returned.
///
/// The token is bound to the address of the [`UserContext`](crate::uspace::UserContext)
/// that produced it. It carries no decoded syndrome and is neither cloneable
/// nor transferable to another CPU. The runtime must publish kernel accounting
/// before giving the token back to that same context for decoding.
#[must_use = "the raw user exit must be completed by the runtime"]
#[derive(Debug)]
pub struct RawUserExit {
    context: *mut (),
    architecture_token: usize,
    not_send: PhantomData<*mut ()>,
}

impl RawUserExit {
    /// Binds a new raw-exit token to the context that just returned.
    pub(crate) fn bind<T>(context: &mut T, architecture_token: usize) -> Self {
        Self {
            context: core::ptr::from_mut(context).cast(),
            architecture_token,
            not_send: PhantomData,
        }
    }

    /// Consumes the token after verifying that its originating context decodes it.
    ///
    /// Returns the architecture-private value captured directly from the user
    /// entry assembly, or zero on architectures whose syndrome registers retain
    /// all information required for decoding.
    ///
    /// # Panics
    ///
    /// Panics when `context` is not the same object that produced this token.
    pub(crate) fn assert_bound_to<T>(self, context: &mut T) -> usize {
        assert_eq!(
            self.context,
            core::ptr::from_mut(context).cast(),
            "raw user exit decoded by a different UserContext",
        );
        self.architecture_token
    }
}

/// A decoded architecture exit consumed by the runtime boundary.
///
/// Decoding is intentionally separate from [`RawUserExit`] production so
/// syndrome-register reads and IRQ-token construction happen only after the
/// runtime has published kernel accounting.
#[must_use = "the decoded user exit must be completed by the runtime"]
#[derive(Debug)]
pub enum DecodedUserExit {
    /// A raw interrupt that the runtime must dispatch exactly once.
    Interrupt(RawUserInterrupt),
    /// A non-interrupt user exit that needs no architecture IRQ dispatch.
    Reason(UserExitReason),
}

/// A completed reason for returning from user execution to the OS.
///
/// The runtime constructs [`UserExitReason::Interrupt`] only after consuming a
/// [`RawUserInterrupt`]. OS code therefore cannot accidentally run its user
/// exception path before IRQ acknowledgement and scheduler-return handling.
#[must_use = "the completed user exit reason must be handled by the OS"]
#[derive(Debug, Clone, Copy)]
pub enum UserExitReason {
    /// An interrupt that the runtime has already dispatched.
    Interrupt,
    /// A system call.
    Syscall,
    /// A page fault.
    PageFault(VirtAddr, PageFaultFlags),
    /// Other kinds of exceptions.
    Exception(ExceptionInfo),
    /// Unknown reason.
    Unknown,
}

/// A generalized kind for [`ExceptionInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionKind {
    #[cfg(target_arch = "x86_64")]
    /// A debug exception.
    Debug,
    /// A breakpoint exception.
    Breakpoint,
    /// An illegal instruction exception.
    IllegalInstruction,
    /// A misaligned access exception.
    Misaligned,
    /// An integer arithmetic exception, i.e. x86 `#DE` (divide-by-zero or the
    /// `INT_MIN / -1` overflow). On x86 this is a real CPU trap that must become
    /// `SIGFPE`; the other architectures do not trap on integer divide-by-zero,
    /// so they never produce this kind.
    ArithmeticError,
    /// Other kinds of exceptions.
    Other,
}

/// Architecture-neutral syndrome fields for user-space exceptions.
///
/// The meaning of each field remains architecture-specific, but this shape
/// gives OS code a single way to log or forward the raw trap details without
/// reaching into every architecture's private register type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExceptionSyndrome {
    /// Raw syndrome/status register value when the architecture exposes one.
    pub raw: u64,
    /// Primary exception class or code.
    pub class: u64,
    /// Architecture-specific instruction syndrome or subcode.
    pub iss: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! assert_not_impl {
        ($tested_type:ty, $tested_trait:path) => {
            const _: fn() = || {
                trait AmbiguousIfImplemented<Marker> {
                    fn check() {}
                }

                impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}

                struct Implemented;
                impl<T: ?Sized + $tested_trait> AmbiguousIfImplemented<Implemented> for T {}

                let _ = <$tested_type as AmbiguousIfImplemented<_>>::check;
            };
        };
    }

    assert_not_impl!(RawUserExit, Send);
    assert_not_impl!(RawUserExit, Clone);
    assert_not_impl!(RawUserExit, Copy);
    assert_not_impl!(RawUserInterrupt, Send);
    assert_not_impl!(RawUserInterrupt, Clone);
    assert_not_impl!(RawUserInterrupt, Copy);
    assert_not_impl!(DecodedUserExit, Send);
    assert_not_impl!(DecodedUserExit, Clone);
    assert_not_impl!(DecodedUserExit, Copy);

    #[test]
    fn raw_interrupt_dispatch_is_consuming() {
        let accessor: fn(RawUserInterrupt) -> bool = RawUserInterrupt::dispatch;
        let _ = accessor;
    }
}
