//! Guest-side helpers for AxVisor inter-VM communication hypercalls.
//!
//! The current Axvisor IVC ABI exchanges output pointers as guest physical
//! addresses. Guest code must translate any stack or static output slot from
//! virtual address to guest physical address before calling these helpers.

use core::fmt;

use crate::HyperCallCode;

/// A guest physical address passed through the Axvisor IVC ABI.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcGuestPhysAddr(usize);

impl IvcGuestPhysAddr {
    /// Creates a guest physical address wrapper.
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    /// Returns the raw address value.
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

/// Error returned by a guest-side IVC hypercall helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IvcHyperCallError {
    /// The current architecture cannot issue Axvisor hypercalls.
    UnsupportedArchitecture,
    /// The hypervisor returned a non-zero status.
    Failed(isize),
}

impl fmt::Display for IvcHyperCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedArchitecture => {
                write!(f, "IVC hypercall is not supported on this architecture")
            }
            Self::Failed(status) => write!(f, "IVC hypercall failed with status {status}"),
        }
    }
}

impl core::error::Error for IvcHyperCallError {}

/// Result type returned by guest-side IVC hypercall helpers.
pub type IvcHyperCallResult<T> = Result<T, IvcHyperCallError>;

/// Publishes a shared-memory IVC channel.
///
/// `shm_base_gpa_ptr` and `shm_size_ptr` are guest physical addresses pointing
/// to writable `usize` slots in the publishing guest. The size slot must contain
/// the requested channel size before the call and will be overwritten with the
/// actual channel size on success.
///
/// # Errors
///
/// Returns [`IvcHyperCallError::UnsupportedArchitecture`] when the target cannot
/// issue Axvisor hypercalls, or [`IvcHyperCallError::Failed`] when Axvisor
/// rejects the request.
pub fn publish_channel(
    key: usize,
    shm_base_gpa_ptr: IvcGuestPhysAddr,
    shm_size_ptr: IvcGuestPhysAddr,
) -> IvcHyperCallResult<()> {
    execute_checked(publish_channel_invocation(
        key,
        shm_base_gpa_ptr,
        shm_size_ptr,
    ))
}

/// Subscribes to a shared-memory IVC channel.
///
/// `shm_base_gpa_ptr` and `shm_size_ptr` are guest physical addresses pointing
/// to writable `usize` slots in the subscribing guest. Axvisor writes the mapped
/// shared page GPA and actual size into those slots on success.
///
/// # Errors
///
/// Returns [`IvcHyperCallError::UnsupportedArchitecture`] when the target cannot
/// issue Axvisor hypercalls, or [`IvcHyperCallError::Failed`] when Axvisor
/// rejects the request.
pub fn subscribe_channel(
    publisher_vm_id: usize,
    key: usize,
    shm_base_gpa_ptr: IvcGuestPhysAddr,
    shm_size_ptr: IvcGuestPhysAddr,
) -> IvcHyperCallResult<()> {
    execute_checked(subscribe_channel_invocation(
        publisher_vm_id,
        key,
        shm_base_gpa_ptr,
        shm_size_ptr,
    ))
}

/// Unpublishes a previously published IVC channel.
///
/// # Errors
///
/// Returns [`IvcHyperCallError::UnsupportedArchitecture`] when the target cannot
/// issue Axvisor hypercalls, or [`IvcHyperCallError::Failed`] when Axvisor
/// rejects the request.
pub fn unpublish_channel(key: usize) -> IvcHyperCallResult<()> {
    execute_checked(HyperCallInvocation::new(
        HyperCallCode::HIVCUnPublishChannel,
        [key, 0, 0, 0, 0, 0],
    ))
}

/// Unsubscribes from a previously subscribed IVC channel.
///
/// # Errors
///
/// Returns [`IvcHyperCallError::UnsupportedArchitecture`] when the target cannot
/// issue Axvisor hypercalls, or [`IvcHyperCallError::Failed`] when Axvisor
/// rejects the request.
pub fn unsubscribe_channel(publisher_vm_id: usize, key: usize) -> IvcHyperCallResult<()> {
    execute_checked(HyperCallInvocation::new(
        HyperCallCode::HIVCUnSubscribChannel,
        [publisher_vm_id, key, 0, 0, 0, 0],
    ))
}

/// Notifies one peer VM that a shared-memory IVC channel has new work.
///
/// `publisher_vm_id` and `key` identify the channel, while `target_vm_id`
/// selects the peer VM to notify. Axvisor validates that the caller and target
/// are both participants of the channel.
///
/// # Errors
///
/// Returns [`IvcHyperCallError::UnsupportedArchitecture`] when the target cannot
/// issue Axvisor hypercalls, or [`IvcHyperCallError::Failed`] when Axvisor
/// rejects the request.
pub fn notify_channel(
    publisher_vm_id: usize,
    key: usize,
    target_vm_id: usize,
) -> IvcHyperCallResult<()> {
    execute_checked(notify_channel_invocation(
        publisher_vm_id,
        key,
        target_vm_id,
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HyperCallInvocation {
    code: HyperCallCode,
    args: [usize; 6],
}

impl HyperCallInvocation {
    const fn new(code: HyperCallCode, args: [usize; 6]) -> Self {
        Self { code, args }
    }
}

fn publish_channel_invocation(
    key: usize,
    shm_base_gpa_ptr: IvcGuestPhysAddr,
    shm_size_ptr: IvcGuestPhysAddr,
) -> HyperCallInvocation {
    HyperCallInvocation::new(
        HyperCallCode::HIVCPublishChannel,
        [
            key,
            shm_base_gpa_ptr.as_usize(),
            shm_size_ptr.as_usize(),
            0,
            0,
            0,
        ],
    )
}

fn subscribe_channel_invocation(
    publisher_vm_id: usize,
    key: usize,
    shm_base_gpa_ptr: IvcGuestPhysAddr,
    shm_size_ptr: IvcGuestPhysAddr,
) -> HyperCallInvocation {
    HyperCallInvocation::new(
        HyperCallCode::HIVCSubscribChannel,
        [
            publisher_vm_id,
            key,
            shm_base_gpa_ptr.as_usize(),
            shm_size_ptr.as_usize(),
            0,
            0,
        ],
    )
}

fn notify_channel_invocation(
    publisher_vm_id: usize,
    key: usize,
    target_vm_id: usize,
) -> HyperCallInvocation {
    HyperCallInvocation::new(
        HyperCallCode::HIVCNotify,
        [publisher_vm_id, key, target_vm_id, 0, 0, 0],
    )
}

fn execute_checked(invocation: HyperCallInvocation) -> IvcHyperCallResult<()> {
    let status = issue_hypercall(invocation)?;
    if status == 0 {
        Ok(())
    } else {
        Err(IvcHyperCallError::Failed(status))
    }
}

#[cfg(target_arch = "aarch64")]
fn issue_hypercall(invocation: HyperCallInvocation) -> IvcHyperCallResult<isize> {
    let mut x0 = invocation.code as usize;
    let args = invocation.args;
    unsafe {
        // The Axvisor AArch64 ABI uses x0 for the hypercall number and return
        // value, and x1-x6 for up to six integer arguments.
        core::arch::asm!(
            "hvc #0",
            inlateout("x0") x0,
            in("x1") args[0],
            in("x2") args[1],
            in("x3") args[2],
            in("x4") args[3],
            in("x5") args[4],
            in("x6") args[5],
            options(nostack),
        );
    }
    Ok(x0 as isize)
}

#[cfg(target_arch = "x86_64")]
fn issue_hypercall(invocation: HyperCallInvocation) -> IvcHyperCallResult<isize> {
    let mut rax = invocation.code as usize;
    let args = invocation.args;
    unsafe {
        // The Axvisor x86_64 ABI uses rax for the hypercall number and return
        // value, and rdi/rsi/rdx/rcx/r8/r9 for up to six integer arguments.
        core::arch::asm!(
            "vmcall",
            inlateout("rax") rax,
            in("rdi") args[0],
            in("rsi") args[1],
            in("rdx") args[2],
            in("rcx") args[3],
            in("r8") args[4],
            in("r9") args[5],
            options(nostack),
        );
    }
    Ok(rax as isize)
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn issue_hypercall(_invocation: HyperCallInvocation) -> IvcHyperCallResult<isize> {
    Err(IvcHyperCallError::UnsupportedArchitecture)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_invocation_matches_axvisor_abi() {
        let invocation = publish_channel_invocation(
            0x33,
            IvcGuestPhysAddr::new(0x8000),
            IvcGuestPhysAddr::new(0x9000),
        );

        assert_eq!(invocation.code, HyperCallCode::HIVCPublishChannel);
        assert_eq!(invocation.args, [0x33, 0x8000, 0x9000, 0, 0, 0]);
    }

    #[test]
    fn subscribe_invocation_matches_axvisor_abi() {
        let invocation = subscribe_channel_invocation(
            1,
            0x33,
            IvcGuestPhysAddr::new(0x8000),
            IvcGuestPhysAddr::new(0x9000),
        );

        assert_eq!(invocation.code, HyperCallCode::HIVCSubscribChannel);
        assert_eq!(invocation.args, [1, 0x33, 0x8000, 0x9000, 0, 0]);
    }

    #[test]
    fn notify_invocation_matches_axvisor_abi() {
        let invocation = notify_channel_invocation(1, 0x33, 2);

        assert_eq!(invocation.code, HyperCallCode::HIVCNotify);
        assert_eq!(invocation.args, [1, 0x33, 2, 0, 0, 0]);
    }
}
