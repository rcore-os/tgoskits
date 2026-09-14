//! `/dev/net/tun` files, following Linux `drivers/net/tun.c`.
//!
//! `TUNSETIFF` binds a file to a TUN or TAP interface, creating the interface
//! when the name is unused. The interface outlives its last file only with
//! `IFF_PERSIST`. Reads and writes move one frame per call through the
//! interface's [`TunShared`] queues.

use alloc::sync::Arc;
use core::any::Any;

use ax_net::{InterfaceKind, TunShared};
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, Pollable, SharedRegistrationSink};
use bytemuck::{Pod, Zeroable};
use linux_raw_sys::general::CAP_NET_ADMIN;

use crate::{
    StarryError,
    mm::UserPtr,
    pseudofs::DeviceOps,
    sync::{IrqMutex, Mutex},
    task::UserTaskRef,
};

const IFF_TUN: u16 = 0x0001;
const IFF_TAP: u16 = 0x0002;
const IFF_NAPI: u16 = 0x0010;
const IFF_NAPI_FRAGS: u16 = 0x0020;
const IFF_MULTI_QUEUE: u16 = 0x0100;
const IFF_PERSIST: u16 = 0x0800;
const IFF_NO_PI: u16 = 0x1000;
/// Shares its bit with `IFF_NO_PI`; `TUNGETIFF` sets it while no socket filter
/// is attached, which is always the case here.
const IFF_NOFILTER: u16 = 0x1000;
const IFF_ONE_QUEUE: u16 = 0x2000;
const IFF_VNET_HDR: u16 = 0x4000;
const IFF_TUN_EXCL: u16 = 0x8000;
/// `TUN_FEATURES` this driver implements; `IFF_ONE_QUEUE` is a no-op in Linux.
const TUN_FEATURES: u16 = IFF_NO_PI | IFF_ONE_QUEUE;
/// `TUN_FEATURES` Linux offers that are not implemented. Refusing them keeps a
/// caller from assuming virtio headers or several queues.
const UNSUPPORTED_FEATURES: u16 = IFF_VNET_HDR | IFF_MULTI_QUEUE | IFF_NAPI | IFF_NAPI_FRAGS;

const TUN_PKT_STRIP: u16 = 0x0001;
const TUN_PI_LEN: usize = 4;
const ETH_HLEN: usize = 14;
const ETH_P_IP: u16 = 0x0800;
const ETH_P_IPV6: u16 = 0x86dd;

const IFNAMSIZ: usize = 16;

// `linux-raw-sys` leaves its `if_tun` bindings disabled; these match every
// supported architecture.
const TUNSETIFF: u32 = 0x4004_54ca;
const TUNSETPERSIST: u32 = 0x4004_54cb;
const TUNGETFEATURES: u32 = 0x8004_54cf;
const TUNGETIFF: u32 = 0x8004_54d2;

/// `struct ifreq` on the supported 64-bit targets: the name, then a 24-byte
/// union whose first two bytes are `ifr_flags`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Ifreq {
    name: [u8; IFNAMSIZ],
    data: [u8; 24],
}

/// Serializes attach, detach and persistence changes, as `rtnl_lock` does.
static TUN_CONTROL: Mutex<()> = Mutex::new(());

#[derive(Clone)]
struct Attachment {
    shared: Arc<TunShared>,
    kind: InterfaceKind,
    /// `TUN_FEATURES` bits the attaching `TUNSETIFF` asked for.
    features: u16,
}

pub(super) struct TunFile {
    attachment: IrqMutex<Option<Attachment>>,
}

impl TunFile {
    pub(super) fn new() -> Self {
        Self {
            attachment: IrqMutex::new(None),
        }
    }

    fn attached(&self) -> VfsResult<Attachment> {
        // Linux answers EBADFD, which the VFS error set cannot express.
        self.attachment
            .lock()
            .clone()
            .ok_or(VfsError::BadFileDescriptor)
    }

    fn set_iff(&self, current: &UserTaskRef, arg: usize) -> VfsResult<usize> {
        let mut ifr = UserPtr::<Ifreq>::from(arg).read(current)?;
        let _control = TUN_CONTROL.lock();
        if self.attachment.lock().is_some() {
            return Err(VfsError::AlreadyExists);
        }
        let attachment = attach(current, ifreq_name(&ifr)?, ifreq_flags(&ifr))?;
        set_ifreq_name(&mut ifr, attachment.shared.name());
        *self.attachment.lock() = Some(attachment);
        // As in Linux, a fault while copying the name back keeps the attachment.
        UserPtr::<Ifreq>::from(arg).write(current, ifr)?;
        Ok(0)
    }

    fn get_iff(current: &UserTaskRef, arg: usize, attachment: &Attachment) -> VfsResult<usize> {
        let mut ifr = Ifreq::zeroed();
        set_ifreq_name(&mut ifr, attachment.shared.name());
        let kind = match attachment.kind {
            InterfaceKind::Tap => IFF_TAP,
            _ => IFF_TUN,
        };
        let persist = if attachment.shared.is_persistent() {
            IFF_PERSIST
        } else {
            0
        };
        let flags = kind | attachment.features | persist | IFF_NOFILTER;
        ifr.data[..2].copy_from_slice(&flags.to_ne_bytes());
        UserPtr::<Ifreq>::from(arg).write(current, ifr)?;
        Ok(0)
    }
}

/// `tun_set_iff` for a single-queue device.
fn attach(current: &UserTaskRef, name: &str, flags: u16) -> VfsResult<Attachment> {
    let existing = if name.is_empty() {
        None
    } else {
        ax_net::interface_by_name(name)
    };
    let (shared, kind) = match existing {
        Some(interface) => {
            if flags & IFF_TUN_EXCL != 0 {
                return Err(VfsError::ResourceBusy);
            }
            let kind = match interface.kind {
                InterfaceKind::Tun if flags & IFF_TUN != 0 => InterfaceKind::Tun,
                InterfaceKind::Tap if flags & IFF_TAP != 0 => InterfaceKind::Tap,
                _ => return Err(VfsError::InvalidInput),
            };
            if flags & UNSUPPORTED_FEATURES != 0 {
                return Err(VfsError::InvalidInput);
            }
            let shared = ax_net::tun_shared_by_name(name).ok_or(VfsError::InvalidInput)?;
            if !shared.try_attach() {
                return Err(VfsError::ResourceBusy);
            }
            (shared, kind)
        }
        None => {
            if !current.as_thread().cred().has_cap(CAP_NET_ADMIN) {
                return Err(VfsError::OperationNotPermitted);
            }
            let kind = if flags & IFF_TUN != 0 {
                InterfaceKind::Tun
            } else if flags & IFF_TAP != 0 {
                InterfaceKind::Tap
            } else {
                return Err(VfsError::InvalidInput);
            };
            if flags & UNSUPPORTED_FEATURES != 0 {
                return Err(VfsError::InvalidInput);
            }
            let shared = ax_net::create_tun(name, kind).map_err(StarryError::from)?;
            if !shared.try_attach() {
                unreachable!("a new TUN interface has a free queue");
            }
            (shared, kind)
        }
    };
    Ok(Attachment {
        shared,
        kind,
        features: flags & TUN_FEATURES,
    })
}

/// The name `__tun_chr_ioctl` sees after terminating it at `IFNAMSIZ - 1`.
fn ifreq_name(ifr: &Ifreq) -> VfsResult<&str> {
    let name = &ifr.name[..IFNAMSIZ - 1];
    let len = name.iter().position(|&byte| byte == 0).unwrap_or(name.len());
    core::str::from_utf8(&name[..len]).map_err(|_| VfsError::InvalidInput)
}

fn ifreq_flags(ifr: &Ifreq) -> u16 {
    u16::from_ne_bytes([ifr.data[0], ifr.data[1]])
}

fn set_ifreq_name(ifr: &mut Ifreq, name: &str) {
    ifr.name.fill(0);
    ifr.name[..name.len()].copy_from_slice(name.as_bytes());
}

/// `skb->protocol` of a routed frame, reported to readers in `tun_pi`.
fn frame_protocol(kind: InterfaceKind, frame: &[u8]) -> u16 {
    match kind {
        InterfaceKind::Tap => frame
            .get(12..14)
            .map_or(0, |ethertype| u16::from_be_bytes([ethertype[0], ethertype[1]])),
        _ => match frame.first().map(|byte| byte >> 4) {
            Some(6) => ETH_P_IPV6,
            _ => ETH_P_IP,
        },
    }
}

impl DeviceOps for TunFile {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        let attachment = self.attached()?;
        let frame = attachment.shared.pop_tx().ok_or(VfsError::WouldBlock)?;
        if attachment.features & IFF_NO_PI != 0 {
            let len = frame.len().min(buf.len());
            buf[..len].copy_from_slice(&frame[..len]);
            return Ok(len);
        }
        // `tun_put_user` rejects a short buffer after the frame left the queue.
        let Some((pi, payload)) = buf.split_first_chunk_mut::<TUN_PI_LEN>() else {
            return Err(VfsError::InvalidInput);
        };
        let flags = if payload.len() < frame.len() {
            TUN_PKT_STRIP
        } else {
            0
        };
        pi[..2].copy_from_slice(&flags.to_ne_bytes());
        pi[2..].copy_from_slice(&frame_protocol(attachment.kind, &frame).to_be_bytes());
        let len = frame.len().min(payload.len());
        payload[..len].copy_from_slice(&frame[..len]);
        Ok(TUN_PI_LEN + len)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        let attachment = self.attached()?;
        let no_pi = attachment.features & IFF_NO_PI != 0;
        let frame = if no_pi {
            buf
        } else {
            buf.get(TUN_PI_LEN..).ok_or(VfsError::InvalidInput)?
        };
        let valid = match attachment.kind {
            InterfaceKind::Tap => frame.len() >= ETH_HLEN,
            // Without `tun_pi` the IP version nibble names the protocol.
            _ => !no_pi || matches!(frame.first().map(|byte| byte >> 4), Some(4 | 6)),
        };
        if !valid {
            return Err(VfsError::InvalidInput);
        }
        if !attachment.shared.is_up() {
            return Err(VfsError::Io);
        }
        attachment.shared.push_rx(frame);
        Ok(buf.len())
    }

    fn ioctl(&self, current: &UserTaskRef, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            TUNGETFEATURES => {
                UserPtr::<u32>::from(arg).write(current, u32::from(IFF_TUN | IFF_TAP | TUN_FEATURES))?;
                Ok(0)
            }
            TUNSETIFF => self.set_iff(current, arg),
            _ => {
                let attachment = self.attached()?;
                match cmd {
                    TUNGETIFF => Self::get_iff(current, arg, &attachment),
                    TUNSETPERSIST => {
                        let _control = TUN_CONTROL.lock();
                        attachment.shared.set_persist(arg != 0);
                        Ok(0)
                    }
                    _ => Err(VfsError::InvalidInput),
                }
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM | NodeFlags::PACKET
    }

    fn close(&self, _exclusive: bool) {
        let _control = TUN_CONTROL.lock();
        let Some(attachment) = self.attachment.lock().take() else {
            return;
        };
        let shared = attachment.shared;
        shared.detach();
        if !shared.is_persistent() {
            shared.mark_dying();
            ax_net::destroy_tun(shared.name());
        }
    }
}

impl Pollable for TunFile {
    fn poll(&self) -> IoEvents {
        let Ok(attachment) = self.attached() else {
            return IoEvents::ERR;
        };
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN | IoEvents::RDNORM, attachment.shared.has_tx());
        events.set(IoEvents::OUT | IoEvents::WRNORM, attachment.shared.is_up());
        events
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        let Ok(attachment) = self.attached() else {
            return;
        };
        if !events.contains(IoEvents::IN) {
            return;
        }
        let readers = attachment.shared.readers();
        unsafe { sink.register_shared(readers, IoEvents::IN) };
        if attachment.shared.has_tx() {
            // SAFETY: the queued frame is published and no lock is held.
            unsafe { readers.wake(IoEvents::IN) };
        }
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn names_stop_before_ifnamsiz() {
        let mut ifr = Ifreq {
            name: [b'a'; IFNAMSIZ],
            data: [0; 24],
        };
        assert_eq!(ifreq_name(&ifr), Ok("aaaaaaaaaaaaaaa"));
        ifr.name[3] = 0;
        assert_eq!(ifreq_name(&ifr), Ok("aaa"));
    }

    #[test]
    fn packet_info_names_the_frame_protocol() {
        assert_eq!(frame_protocol(InterfaceKind::Tun, &[0x45]), ETH_P_IP);
        assert_eq!(frame_protocol(InterfaceKind::Tun, &[0x60]), ETH_P_IPV6);
        let mut frame = [0u8; ETH_HLEN];
        frame[12..].copy_from_slice(&0x0806u16.to_be_bytes());
        assert_eq!(frame_protocol(InterfaceKind::Tap, &frame), 0x0806);
    }
}
