//! `/dev/net/tun` character device.
//!
//! Opening this node yields an unattached clone; a `TUNSETIFF` ioctl binds it to
//! a named TUN interface (creating it on first use). From then on `read(2)`
//! returns packets the stack routed to the interface and `write(2)` injects
//! packets from userspace into the stack. Readiness is driven by the shared
//! [`TunShared`] poll set, so `poll(2)`/`epoll` and blocking reads work through
//! the generic device poll path.
//!
//! # Packet Information Header
//!
//! Unless `IFF_NO_PI` was requested, each frame is framed by a 4-byte
//! `struct tun_pi { __u16 flags; __be16 proto; }`. Reads prepend it; writes
//! strip it. With `IFF_NO_PI` the fd carries bare IP packets.

use alloc::{
    string::{String, ToString},
    sync::Arc,
};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_errno::AxError;
use ax_net::{InterfaceKind, TunShared};
use ax_sync::spin::SpinNoIrq as Mutex;
use ax_task::current;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, PollSet, Pollable};

use crate::{mm::UserPtr, pseudofs::DeviceOps, task::AsThread};

/// Linux `IFF_TUN`: layer-3 (IP) device.
const IFF_TUN: u16 = 0x0001;
/// Linux `IFF_TAP`: layer-2 (Ethernet) device.
const IFF_TAP: u16 = 0x0002;
/// Linux `IFF_NO_PI`: do not prepend the 4-byte packet-information header.
const IFF_NO_PI: u16 = 0x1000;
/// `IFF_PERSIST`: keep the device after the last fd closes (Linux `if_tun.h`).
/// Stored at device level in [`TunShared`], not per-fd.
const IFF_PERSIST: u16 = 0x0800;
/// Flags TUNSETIFF may set that this driver understands. Anything outside the
/// device-type and PI bits is rejected so callers do not assume unsupported
/// offloads (checksum/vnet-hdr/multi-queue) took effect.
const TUN_SUPPORTED_FLAGS: u16 = IFF_TUN | IFF_TAP | IFF_NO_PI;

/// `struct tun_pi` size and `ETH_P_IP` used to frame reads.
const TUN_PI_LEN: usize = 4;
const ETH_P_IP: u16 = 0x0800;
const ETH_P_IPV6: u16 = 0x86dd;
/// `TUN_PKT_STRIP` bit in `struct tun_pi::flags`: indicates the frame was
/// truncated because the read buffer was too small (tun.c:tun_put_user).
const TUN_PKT_STRIP: u16 = 0x0001;

/// Offset of `ifr_flags`/`ifr_name` in `struct ifreq`. The name occupies the
/// first 16 bytes; the flags live in the `ifr_ifru` union that follows.
const IFREQ_NAME_LEN: usize = 16;

/// Per-open state of a `/dev/net/tun` file.
struct TunFileState {
    /// Bound interface, or `None` before `TUNSETIFF`.
    attached: Option<Arc<TunShared>>,
    /// `IFF_*` flags negotiated by `TUNSETIFF`.
    flags: u16,
    /// Set by `close()` so a concurrent `TUNSETIFF` that created a device but
    /// has not yet written it back can detect the file is being torn down and
    /// clean up the freshly-created interface rather than leaking it.
    closing: bool,
}

pub struct TunFile {
    state: Mutex<TunFileState>,
    /// Poll set exposed to `poll(2)` before any attachment, so an unattached fd
    /// still registers cleanly. Once attached, the shared poll set is used.
    unattached_poll: Arc<PollSet>,
    /// Serializes concurrent `TUNSETIFF` calls on the same fd. Linux uses
    /// `tun_lock`/`rtnl_lock` for this; we use a simple flag because `set_iff`
    /// must release `state`'s `SpinNoIrq` before calling into the network
    /// service (which holds its own lock), yet must prevent two concurrent
    /// callers from both passing the `attached.is_none()` check and then racing
    /// to write back two different `Arc<TunShared>` handles.
    setting_iff: AtomicBool,
}

impl TunFile {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TunFileState {
                attached: None,
                flags: 0,
                closing: false,
            }),
            unattached_poll: Arc::new(PollSet::new()),
            setting_iff: AtomicBool::new(false),
        }
    }

    fn no_pi(flags: u16) -> bool {
        flags & IFF_NO_PI != 0
    }

    fn is_tap(flags: u16) -> bool {
        flags & IFF_TAP != 0
    }

    /// Reads the interface name (NUL-terminated, 16 bytes) from an `ifreq`.
    fn read_ifr_name(arg: usize) -> VfsResult<String> {
        let bytes = UserPtr::<u8>::from(arg).get_as_mut_slice(IFREQ_NAME_LEN)?;
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        core::str::from_utf8(&bytes[..end])
            .map(str::to_string)
            .map_err(|_| VfsError::InvalidInput)
    }

    /// Reads `ifr_flags` (a `short` right after the 16-byte name) from an
    /// `ifreq`.
    fn read_ifr_flags(arg: usize) -> VfsResult<u16> {
        let flags = UserPtr::<u16>::from(arg + IFREQ_NAME_LEN).get_as_mut()?;
        Ok(*flags)
    }

    fn write_ifr_flags(arg: usize, flags: u16) -> VfsResult<()> {
        *UserPtr::<u16>::from(arg + IFREQ_NAME_LEN).get_as_mut()? = flags;
        Ok(())
    }

    fn write_ifr_name(arg: usize, name: &str) -> VfsResult<()> {
        let dst = UserPtr::<u8>::from(arg).get_as_mut_slice(IFREQ_NAME_LEN)?;
        dst.fill(0);
        let bytes = name.as_bytes();
        let len = bytes.len().min(IFREQ_NAME_LEN - 1);
        dst[..len].copy_from_slice(&bytes[..len]);
        Ok(())
    }

    /// `TUNSETIFF`: create or bind an interface to this fd.
    fn set_iff(&self, arg: usize) -> VfsResult<usize> {
        // Serialize concurrent TUNSETIFF calls on this fd. Without this, two
        // concurrent callers can both pass the `attached.is_none()` check (done
        // outside `state`'s lock to avoid holding a SpinNoIrq across the
        // network-service call), both create/find a device, and the second
        // write-back overwrites the first, leaking the first device handle.
        // Linux uses tun_lock / rtnl_lock for the same purpose.
        if self
            .setting_iff
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(VfsError::ResourceBusy);
        }
        let result = self.set_iff_inner(arg);
        self.setting_iff.store(false, Ordering::Release);
        result
    }

    fn set_iff_inner(&self, arg: usize) -> VfsResult<usize> {
        let name = Self::read_ifr_name(arg)?;
        let flags = Self::read_ifr_flags(arg)?;

        if flags & !TUN_SUPPORTED_FLAGS != 0 {
            return Err(VfsError::InvalidInput);
        }
        let kind = match (flags & IFF_TUN != 0, flags & IFF_TAP != 0) {
            // Exactly one of TUN/TAP must be set.
            (true, false) => InterfaceKind::Tun,
            (false, true) => InterfaceKind::Tap,
            _ => return Err(VfsError::InvalidInput),
        };

        // Linux `tun_set_iff` requires `CAP_NET_ADMIN` to create a new device,
        // and gates attaching to an existing one on `tun_not_capable()` (the
        // device owner/group, or `CAP_NET_ADMIN`). This driver does not track a
        // per-device owner (no `TUNSETOWNER`/`TUNSETGROUP`), so both the create
        // and the attach path require `CAP_NET_ADMIN`; without it, EPERM.
        if !current().as_thread().cred().has_cap_net_admin() {
            return Err(VfsError::OperationNotPermitted);
        }

        // Linux returns EINVAL when TUNSETIFF is issued on an already-bound fd.
        // The check runs under the lock, which is then released before any
        // network-service call (that path takes the service lock, which must not
        // nest under this spinlock). Concurrent callers on the same fd are
        // excluded by the `setting_iff` flag checked in `set_iff`.
        if self.state.lock().attached.is_some() {
            return Err(VfsError::InvalidInput);
        }

        // An empty name is always a create request: Linux hands it to the netdev
        // name allocator, which returns the first free `tun%d`/`tap%d`.
        // `create_tun` performs that allocation atomically, so two empty-name
        // creates get distinct names (tun0, tun1) instead of colliding on tun0.
        // A non-empty name binds to a matching existing interface, otherwise
        // creates one under that name.
        // Track whether this ioctl created the interface: on any failure before
        // the attachment is recorded, a created device must be destroyed (it has
        // no other owner), whereas a pre-existing one is only detached so it
        // survives for a later `TUNSETIFF`.
        let (shared, created) = if name.is_empty() {
            (
                ax_net::create_tun(alloc::string::String::new(), kind)?,
                true,
            )
        } else {
            match ax_net::interface_by_name(&name) {
                Some(info) if info.kind == kind => (
                    ax_net::tun_shared_by_name(&name).ok_or(VfsError::InvalidInput)?,
                    false,
                ),
                Some(_) => return Err(VfsError::InvalidInput),
                None => (ax_net::create_tun(name.clone(), kind)?, true),
            }
        };

        // Claim the interface's single queue. Binding a second fd to an
        // already-attached non-multi-queue interface is rejected with EBUSY,
        // matching Linux `tun_attach` (tun.c: `!(tun->flags & IFF_MULTI_QUEUE) &&
        // tun->numqueues == 1`). A freshly created device is unclaimed, so its
        // creator always succeeds. `try_attach` also fails for a `Dying` device,
        // preventing attachment after `mark_dying()` + `destroy_tun()`.
        if !shared.try_attach() {
            return Err(VfsError::ResourceBusy);
        }

        // Hand the claimed interface to the write-back + record tail. Keeping
        // the write-back behind a closure lets the axtest harness inject a
        // forced failure (real userspace cannot fault only the write-back; see
        // `tun_rollback_*_for_test`) and observe that the claim is rolled back.
        self.finish_set_iff(&shared, created, flags, |name| {
            Self::write_ifr_name(arg, name)
        })
    }

    /// Completes `TUNSETIFF` once the interface's queue is already claimed:
    /// writes the allocated name back to userspace, then either records the
    /// attachment or rolls the claim back.
    ///
    /// `write_back` receives the allocated interface name and performs the
    /// userspace write-back (production passes [`Self::write_ifr_name`]). Any
    /// error - a faulting `ifr` pointer in production, or an injected failure
    /// under axtest - must undo the claim: leaving the device `Attached` would
    /// strand the name in EBUSY forever, and a freshly created device would leak.
    fn finish_set_iff(
        &self,
        shared: &Arc<TunShared>,
        created: bool,
        flags: u16,
        write_back: impl FnOnce(&str) -> VfsResult<()>,
    ) -> VfsResult<usize> {
        // The write-back to userspace can fault (bad `ifr` pointer). If it does
        // after the queue is already claimed, the attachment must be rolled back
        // or the device is stranded in `Attached` forever (every later
        // `TUNSETIFF` on the name then fails `try_attach` with EBUSY), and a
        // freshly created device would leak entirely. Roll back exactly as the
        // concurrent-close path below.
        if let Err(e) = write_back(shared.name()) {
            Self::rollback_claim(shared, created);
            return Err(e);
        }
        {
            let mut state = self.state.lock();
            if !state.closing {
                state.flags = flags;
                state.attached = Some(shared.clone());
                return Ok(0);
            }
        }
        // `close()` ran concurrently and marked the fd dying before we could
        // record the attachment. Roll back the just-claimed interface, then
        // signal the caller as if close beat the ioctl (EBADFD mirrors Linux
        // returning -EINVAL on a closed tun fd).
        Self::rollback_claim(shared, created);
        Err(VfsError::BadFileDescriptor)
    }

    /// Undoes a successful `try_attach()` when `set_iff_inner` fails before it
    /// records the attachment in `state`. A device this ioctl created is torn
    /// down (`mark_dying` first to close the detach→destroy TOCTOU window, so a
    /// racing `TUNSETIFF` cannot grab a device about to vanish); a pre-existing
    /// device is only detached so it survives for a later attach.
    ///
    /// Runs with no `SpinNoIrq` held: `mark_dying`/`detach` and `destroy_tun`
    /// take sleeping service/registry locks (and `destroy_tun` joins the device
    /// workers); sleeping under a `SpinNoIrq` aborts with an atomic-context-sleep
    /// panic. This matches the lock discipline already used in `close()`.
    fn rollback_claim(shared: &Arc<TunShared>, created: bool) {
        if created {
            shared.mark_dying();
            shared.detach();
            ax_net::destroy_tun(shared.name());
        } else {
            shared.detach();
        }
    }

    /// `TUNGETIFF`: report the bound interface name and flags.
    ///
    /// Returns the negotiated `IFF_TUN`/`IFF_TAP`/`IFF_NO_PI` bits OR'd with
    /// the device-level `IFF_PERSIST` flag if set, matching Linux tun.c:2717:
    /// `return tun->flags & (TUN_FEATURES | IFF_PERSIST | IFF_TUN | IFF_TAP)`.
    fn get_iff(&self, arg: usize) -> VfsResult<usize> {
        // Snapshot name, per-fd flags, and device persist under the lock, then
        // release before touching user memory (faulting under SpinNoIrq wedges
        // the CPU).
        let (name, flags) = {
            let state = self.state.lock();
            let shared = state.attached.as_ref().ok_or(VfsError::InvalidInput)?;
            let mut flags = state.flags;
            if shared.is_persistent() {
                flags |= IFF_PERSIST;
            }
            (shared.name().to_string(), flags)
        };
        Self::write_ifr_name(arg, &name)?;
        Self::write_ifr_flags(arg, flags)?;
        Ok(0)
    }

    /// `TUNSETPERSIST`: keep the interface alive after the fd closes.
    ///
    /// Linux stores `IFF_PERSIST` in `tun->flags` (device-level), not per-fd.
    /// We mirror that by storing the flag in [`TunShared`] so all fds sharing
    /// the device observe the same value.
    fn set_persist(&self, arg: usize) -> VfsResult<usize> {
        // Making a device outlive its fd is a privileged network-administration
        // operation, gated on `CAP_NET_ADMIN` like the create/attach that
        // established the device (a non-privileged holder of an inherited fd
        // must not be able to turn a transient device into a persistent one).
        if !current().as_thread().cred().has_cap_net_admin() {
            return Err(VfsError::OperationNotPermitted);
        }
        let shared = {
            let state = self.state.lock();
            state.attached.clone().ok_or(VfsError::InvalidInput)?
        };
        shared.set_persist(arg != 0);
        Ok(0)
    }

    /// `TUNGETFEATURES`: report the flag bits this driver honors.
    fn get_features(arg: usize) -> VfsResult<usize> {
        *UserPtr::<u32>::from(arg).get_as_mut()? = TUN_SUPPORTED_FLAGS as u32;
        Ok(0)
    }
}

/// Drives `finish_set_iff` with an injected write-back failure so the axtest
/// harness can prove `rollback_claim` undoes the queue claim. Returns the
/// interface handle (whose `AttachState` the caller inspects) alongside the
/// ioctl result, which must be the injected error.
///
/// The write-back is forced to fail with the caller-supplied `fault`, standing
/// in for the `write_ifr_name` EFAULT that a bad `ifr` pointer would raise but
/// that single-threaded userspace cannot reach on its own: `read_ifr_name`
/// validates the same 16-byte region (READ|WRITE) before `try_attach`, so a
/// pointer bad enough to fault the write-back faults the earlier read first.
#[cfg(axtest)]
fn drive_failed_write_back(created: bool, fault: VfsError) -> (Arc<TunShared>, VfsResult<usize>) {
    let file = TunFile::new();
    let shared = TunShared::new_detached_for_test(alloc::string::String::from("tuntest0"));
    // Mirror `set_iff_inner`: the queue is claimed before the write-back.
    assert!(shared.try_attach());
    let result = file.finish_set_iff(&shared, created, IFF_TUN | IFF_NO_PI, |_name| Err(fault));
    // The fd must not have recorded the attachment on the failing path.
    assert!(file.state.lock().attached.is_none());
    (shared, result)
}

/// axtest: a `TUNSETIFF` that created a new interface but then failed the
/// write-back must destroy it (`mark_dying` + `detach`), not leave it stranded
/// in `Attached` (which would leak the device and pin the name in EBUSY).
///
/// Old-red / new-green: with `rollback_claim` in place the device ends in the
/// terminal `Dying` state and this returns `true`. Revert the write-back guard
/// to the bare `write_ifr_name(arg, shared.name())?` (no `rollback_claim`) and
/// the device is left `Attached`, so `is_dying_for_test()` is false and this
/// returns `false` - the axtest case flips red.
#[cfg(axtest)]
pub(crate) fn tun_rollback_destroys_created_device_for_test() -> bool {
    let (shared, result) = drive_failed_write_back(true, VfsError::BadAddress);
    // The ioctl surfaces the injected write-back error unchanged.
    result == Err(VfsError::BadAddress)
        // A created device is fully torn down: terminal `Dying`, never left
        // claimed, and permanently unattachable so no racer can revive it.
        && shared.is_dying_for_test()
        && !shared.is_attached_for_test()
        && !shared.try_attach()
}

/// axtest: a `TUNSETIFF` that attached to a *pre-existing* interface but then
/// failed the write-back must only `detach` it (return the slot to `Free`) so
/// the device survives for a later `TUNSETIFF`, and must not mark it `Dying`.
///
/// Old-red / new-green: with `rollback_claim` the slot returns to `Free` and a
/// fresh `try_attach()` succeeds, so this returns `true`. Without the rollback
/// the slot stays `Attached`, the follow-up `try_attach()` fails, and this
/// returns `false`.
#[cfg(axtest)]
pub(crate) fn tun_rollback_detaches_existing_device_for_test() -> bool {
    let (shared, result) = drive_failed_write_back(false, VfsError::BadAddress);
    result == Err(VfsError::BadAddress)
        // A pre-existing device is only released: not dying, not still claimed,
        // and reattachable by a subsequent TUNSETIFF.
        && !shared.is_dying_for_test()
        && !shared.is_attached_for_test()
        && shared.try_attach()
}

/// axtest: the concurrent-close rollback path. If `close()` marks the fd dying
/// (`closing = true`) after `try_attach` but before the attachment is recorded,
/// `finish_set_iff` must roll the claim back and report `EBADFD`, never leaving
/// the just-created device leaked in `Attached`.
///
/// Old-red / new-green: with the `closing`-guarded record + `rollback_claim`
/// this returns `true`. Drop the `closing` check (unconditionally record the
/// attachment) and the fd would latch a device it is tearing down; drop the
/// rollback and a created device leaks in `Attached` - either way this returns
/// `false`.
#[cfg(axtest)]
pub(crate) fn tun_rollback_on_concurrent_close_for_test() -> bool {
    let file = TunFile::new();
    let shared = TunShared::new_detached_for_test(alloc::string::String::from("tuntest1"));
    assert!(shared.try_attach());
    // Simulate close() winning the race: it flips `closing` before the ioctl
    // records the attachment.
    file.state.lock().closing = true;
    // Write-back succeeds this time; the loss happens at the `closing` check.
    let result = file.finish_set_iff(&shared, true, IFF_TUN | IFF_NO_PI, |_name| Ok(()));
    result == Err(VfsError::BadFileDescriptor)
        && file.state.lock().attached.is_none()
        && shared.is_dying_for_test()
        && !shared.try_attach()
}

impl DeviceOps for TunFile {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        let (shared, no_pi, is_tap) = {
            let state = self.state.lock();
            let shared = state.attached.as_ref().ok_or(VfsError::BadFileDescriptor)?;
            (
                shared.clone(),
                Self::no_pi(state.flags),
                Self::is_tap(state.flags),
            )
        };

        let Some(packet) = shared.pop_tx() else {
            // No queued packet: let the generic file layer block or return
            // EAGAIN based on O_NONBLOCK.
            return Err(AxError::WouldBlock);
        };

        if no_pi {
            let len = packet.len().min(buf.len());
            buf[..len].copy_from_slice(&packet[..len]);
            Ok(len)
        } else {
            // Prepend struct tun_pi { __u16 flags; __be16 proto } (tun.c:tun_put_user).
            // For a layer-2 TAP the protocol is the frame's ethertype (bytes 12..14);
            // for a layer-3 TUN it is inferred from the IP version nibble.
            if buf.len() < TUN_PI_LEN {
                // Buffer cannot even hold the PI header; Linux returns -EINVAL.
                return Err(VfsError::InvalidInput);
            }
            let proto = if is_tap {
                packet
                    .get(12..14)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]))
                    .unwrap_or(ETH_P_IP)
            } else {
                match packet.first().map(|b| b >> 4) {
                    Some(6) => ETH_P_IPV6,
                    _ => ETH_P_IP,
                }
            };
            let payload_room = buf.len() - TUN_PI_LEN;
            let truncated = packet.len() > payload_room;
            let mut flags: u16 = 0;
            if truncated {
                // Packet does not fit: set TUN_PKT_STRIP so the reader knows
                // the frame was silently truncated (tun.c:2093 `pi.flags |= TUN_PKT_STRIP`).
                flags |= TUN_PKT_STRIP;
            }
            let mut pi = [0u8; TUN_PI_LEN];
            pi[0..2].copy_from_slice(&flags.to_ne_bytes());
            pi[2..4].copy_from_slice(&proto.to_be_bytes());
            buf[..TUN_PI_LEN].copy_from_slice(&pi);
            let len = packet.len().min(payload_room);
            buf[TUN_PI_LEN..TUN_PI_LEN + len].copy_from_slice(&packet[..len]);
            Ok(TUN_PI_LEN + len)
        }
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        let (shared, no_pi) = {
            let state = self.state.lock();
            let shared = state.attached.as_ref().ok_or(VfsError::BadFileDescriptor)?;
            (shared.clone(), Self::no_pi(state.flags))
        };

        let payload = if no_pi {
            buf
        } else {
            // Strip the 4-byte packet-information header.
            buf.get(TUN_PI_LEN..).ok_or(VfsError::InvalidInput)?
        };
        if payload.is_empty() {
            return Err(VfsError::InvalidInput);
        }

        // Report the full user-supplied length as consumed even when the packet
        // is dropped for exceeding the MTU, matching Linux tun's behavior of not
        // failing a write on an over-length datagram.
        shared.push_rx(payload);
        Ok(buf.len())
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            TUNSETIFF => self.set_iff(arg),
            TUNGETIFF => self.get_iff(arg),
            TUNSETPERSIST => self.set_persist(arg),
            TUNGETFEATURES => Self::get_features(arg),
            _ => Err(VfsError::NotATty),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }

    fn close(&self, _exclusive: bool) {
        // Mark the fd closing and take the attached handle atomically under the
        // lock. The `closing` flag races with a concurrent `TUNSETIFF` that has
        // passed the `attached.is_none()` check but not yet written back: that
        // path re-acquires the lock and checks `closing`, so it will clean up
        // the just-created interface rather than leak it on a dead fd.
        let shared = {
            let mut state = self.state.lock();
            state.closing = true;
            state.attached.take()
        };
        if let Some(shared) = shared {
            // IFF_PERSIST is a device-level flag stored in TunShared (matching
            // Linux `tun->flags & IFF_PERSIST`). Read it after dropping the fd
            // state lock.
            let persist = shared.is_persistent();
            if !persist {
                // Mark the device dying before detaching. This closes a TOCTOU
                // window between `detach()` (clears the attach slot) and
                // `destroy_tun()` (removes the device from the router): without
                // the dying marker, a concurrent `TUNSETIFF` on another fd can
                // find the device by name, call `try_attach()` while the slot is
                // momentarily free, and obtain a handle to a device that is about
                // to be destroyed. `mark_dying()` makes `try_attach()` fail for
                // any `AttachState`, so no new fd can acquire the device.
                shared.mark_dying();
            }
            // Release the single-queue claim so a persistent interface can be
            // reattached by a later `TUNSETIFF`.
            shared.detach();
            // A non-persistent interface is removed from the control plane when
            // its last fd closes; a persistent one keeps living in the router.
            if !persist {
                ax_net::destroy_tun(shared.name());
            }
        }
    }
}

impl Pollable for TunFile {
    fn poll(&self) -> IoEvents {
        let state = self.state.lock();
        let mut events = IoEvents::empty();
        if let Some(shared) = state.attached.as_ref() {
            // Writable whenever attached (the RX queue drops on overflow, so it
            // never blocks); readable when a routed packet is queued.
            events |= IoEvents::OUT;
            events.set(IoEvents::IN, shared.has_tx());
        }
        events
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: IoEvents) {
        // Snapshot the attachment under the lock, then drop it before arming the
        // poll set or waking: `PollSet::register`/waker execution must not run
        // under this `SpinNoIrq` (preemption and IRQs disabled), and the waker
        // path may re-enter poll registration.
        let shared = self.state.lock().attached.clone();
        match shared {
            Some(shared) => {
                // SAFETY: `PollSet::register` must not be called from a
                // preempt/IRQ-disabled context nor while holding the state
                // `SpinNoIrq` lock, because its waker path may re-enter poll
                // registration. The `attached` snapshot above dropped that lock
                // before this call, so the precondition holds.
                unsafe { shared.poll_set().register(context.waker(), events) };
                if events.contains(IoEvents::IN) && shared.has_tx() {
                    context.waker().wake_by_ref();
                }
            }
            None => {
                // An unattached fd has nothing to signal yet; arm on the local
                // set so a later TUNSETIFF-driven wake path stays consistent.
                // SAFETY: as above - the state lock was released before this
                // `register`, satisfying `PollSet::register`'s no-preempt /
                // IRQ-enabled precondition.
                unsafe { self.unattached_poll.register(context.waker(), events) };
            }
        }
    }
}

// linux-raw-sys gates the `if_tun` module behind a feature the kernel does not
// enable, so the ioctl numbers are declared here. They are identical across all
// supported architectures (verified against the x86_64/aarch64/riscv64/
// loongarch64 tables).
const TUNSETIFF: u32 = 0x400454ca;
const TUNGETIFF: u32 = 0x800454d2;
const TUNSETPERSIST: u32 = 0x400454cb;
const TUNGETFEATURES: u32 = 0x800454cf;
