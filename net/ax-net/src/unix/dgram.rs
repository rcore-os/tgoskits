//! Unix datagram transport.
//!
//! Datagram sockets use async channels to preserve message boundaries and pass
//! ancillary data together with each packet. Bound endpoints publish a sender in
//! the Unix namespace, while connected socket pairs keep direct peer channels
//! for fast local delivery.
//!
//! # Delivery Semantics
//!
//! Each send builds one `Packet` containing payload, cmsg data, and sender
//! address. A receiver consumes exactly one packet per recv call, which keeps
//! Unix datagram behavior separate from the byte-stream logic in
//! `stream.rs`.
//!
//! # Readiness
//!
//! Bound sockets and socketpairs both carry a `PollSet`. Senders wake the
//! receiver after enqueueing a packet; poll registration never touches the
//! global smoltcp socket set.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::task::Context;

use async_channel::TryRecvError;
use async_trait::async_trait;
use ax_errno::{AxError, AxResult};
use ax_io::{Read, Write};
use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet, Pollable};
use spin::RwLock;

use crate::{
    CMsgData, RecvFlags, RecvOptions, SendOptions, SocketAddrEx,
    general::GeneralOptions,
    options::{Configurable, GetSocketOption, SetSocketOption, UnixCredentials},
    unix::{Transport, TransportOps, UnixSocketAddr, with_slot},
};

struct Packet {
    /// Datagram payload.
    data: Vec<u8>,
    /// Ancillary messages carried with this datagram.
    cmsg: Vec<CMsgData>,
    /// Sender address reported by recvmsg.
    sender: UnixSocketAddr,
}

struct Channel {
    /// Sender side of the peer's datagram queue.
    data_tx: async_channel::Sender<Packet>,
    /// Poll set woken when data is queued.
    poll_update: Arc<PollSet>,
}

pub struct Bind {
    /// Sender published in the Unix namespace for this bound address.
    data_tx: async_channel::Sender<Packet>,
    /// Poll set associated with the receiver bound at this address.
    poll_update: Arc<PollSet>,
}
impl Bind {
    fn connect(&self) -> Channel {
        let tx = self.data_tx.clone();
        Channel {
            data_tx: tx,
            poll_update: self.poll_update.clone(),
        }
    }
}

/// Datagram transport for Unix domain sockets.
pub struct DgramTransport {
    /// Receiver installed when the socket is bound or paired.
    data_rx: Mutex<Option<(async_channel::Receiver<Packet>, Arc<PollSet>)>>,
    /// Direct peer channel for connected datagram sockets.
    connected: RwLock<Option<Channel>>,
    /// Address reported as sender on outgoing datagrams.
    local_addr: RwLock<UnixSocketAddr>,
    /// Poll set for local state changes.
    poll_state: Arc<PollSet>,
    /// Shared socket options.
    general: GeneralOptions,
    /// Creator pid used for SO_PEERCRED-style reporting.
    pid: u32,
}
impl DgramTransport {
    /// Create a new unconnected datagram transport.
    pub fn new(pid: u32) -> Self {
        DgramTransport {
            data_rx: Mutex::new(None),
            connected: RwLock::new(None),
            local_addr: RwLock::new(UnixSocketAddr::Unnamed),
            poll_state: Arc::default(),
            general: GeneralOptions::new(2, 1, 0), // SOCK_DGRAM
            pid,
        }
    }

    fn new_connected(
        data_rx: (async_channel::Receiver<Packet>, Arc<PollSet>),
        connected: Channel,
        pid: u32,
    ) -> Self {
        DgramTransport {
            data_rx: Mutex::new(Some(data_rx)),
            connected: RwLock::new(Some(connected)),
            local_addr: RwLock::new(UnixSocketAddr::Unnamed),
            poll_state: Arc::default(),
            general: GeneralOptions::new(2, 1, 0), // SOCK_DGRAM
            pid,
        }
    }

    /// Create a connected pair of datagram transports.
    pub fn new_pair(pid: u32) -> (Self, Self) {
        let (tx1, rx1) = async_channel::unbounded();
        let (tx2, rx2) = async_channel::unbounded();
        let poll1 = Arc::new(PollSet::new());
        let poll2 = Arc::new(PollSet::new());
        let transport1 = DgramTransport::new_connected(
            (rx1, poll1.clone()),
            Channel {
                data_tx: tx2,
                poll_update: poll2.clone(),
            },
            pid,
        );
        let transport2 = DgramTransport::new_connected(
            (rx2, poll2.clone()),
            Channel {
                data_tx: tx1,
                poll_update: poll1.clone(),
            },
            pid,
        );
        (transport1, transport2)
    }
}

impl Configurable for DgramTransport {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> AxResult<bool> {
        use GetSocketOption as O;

        if self.general.get_option_inner(opt)? {
            return Ok(true);
        }

        match opt {
            O::PassCredentials(_) => {}
            O::PeerCredentials(cred) => {
                // Datagram sockets are stateless and do not have a peer, so we
                // return the credentials of the process that created the
                // socket.
                **cred = UnixCredentials::new(self.pid);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> AxResult<bool> {
        use SetSocketOption as O;

        if self.general.set_option_inner(opt)? {
            return Ok(true);
        }

        match opt {
            O::PassCredentials(_) => {}
            _ => return Ok(false),
        }
        Ok(true)
    }
}
#[async_trait]
impl TransportOps for DgramTransport {
    fn bind(&self, slot: &super::BindSlot, local_addr: &UnixSocketAddr) -> AxResult {
        let mut slot = slot.dgram.lock();
        if slot.is_some() {
            return Err(AxError::AddrInUse);
        }
        let mut guard = self.data_rx.lock();
        if guard.is_some() {
            return Err(AxError::InvalidInput);
        }
        let (tx, rx) = async_channel::unbounded();
        let poll_update = Arc::new(PollSet::new());
        *slot = Some(Bind {
            data_tx: tx,
            poll_update: poll_update.clone(),
        });
        *guard = Some((rx, poll_update));
        self.local_addr.write().clone_from(local_addr);
        drop(guard);
        drop(slot);
        // Datagram bind state is published before waking pollers.
        unsafe { self.poll_state.wake(IoEvents::IN | IoEvents::OUT) };
        Ok(())
    }

    fn connect(&self, slot: &super::BindSlot, _local_addr: &UnixSocketAddr) -> AxResult {
        let mut guard = self.connected.write();
        if guard.is_some() {
            return Err(AxError::AlreadyConnected);
        }
        *guard = Some(
            slot.dgram
                .lock()
                .as_ref()
                .ok_or(AxError::NotConnected)?
                .connect(),
        );
        drop(guard);
        // Connected peer state is published before waking pollers.
        unsafe { self.poll_state.wake(IoEvents::IN | IoEvents::OUT) };
        Ok(())
    }

    async fn accept(&self) -> AxResult<(Transport, UnixSocketAddr)> {
        Err(AxError::InvalidInput)
    }

    fn send(&self, mut src: impl Read, options: SendOptions) -> AxResult<usize> {
        let mut message = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => message.extend_from_slice(&buf[..n]),
                Err(e) => return Err(e),
            }
        }
        let len = message.len();
        let packet = Packet {
            data: message,
            cmsg: options.cmsg,
            sender: self.local_addr.read().clone(),
        };

        let wake_poll = if let Some(addr) = options.to {
            let addr = addr.into_unix()?;
            with_slot(&addr, |slot| {
                if let Some(bind) = slot.dgram.lock().as_ref() {
                    bind.data_tx
                        .try_send(packet)
                        .map_err(|_| AxError::BrokenPipe)?;
                    Ok(bind.poll_update.clone())
                } else {
                    Err(AxError::NotConnected)
                }
            })?
        } else if let Some(chan) = self.connected.read().as_ref() {
            chan.data_tx
                .try_send(packet)
                .map_err(|_| AxError::BrokenPipe)?;
            chan.poll_update.clone()
        } else {
            return Err(AxError::NotConnected);
        };
        // Datagram packet is queued before waking the receiver.
        unsafe { wake_poll.wake(IoEvents::IN) };
        Ok(len)
    }

    fn recv(&self, mut dst: impl Write, mut options: RecvOptions) -> AxResult<usize> {
        let extra_nb = options.flags.contains(RecvFlags::DONTWAIT);
        self.general.recv_poller_with(self, extra_nb, move || {
            let mut guard = self.data_rx.lock();
            let Some((rx, _)) = guard.as_mut() else {
                return Err(AxError::NotConnected);
            };

            let Packet { data, cmsg, sender } = match rx.try_recv() {
                Ok(packet) => packet,
                Err(TryRecvError::Empty) => {
                    return Err(AxError::WouldBlock);
                }
                Err(TryRecvError::Closed) => {
                    return Ok(0);
                }
            };
            let count = dst.write(&data)?;
            if count < data.len() {
                warn!("UDP message truncated: {} -> {} bytes", data.len(), count);
            }

            if let Some(from) = options.from.as_mut() {
                **from = SocketAddrEx::Unix(sender);
            }
            if let Some(dst) = options.cmsg.as_mut() {
                dst.extend(cmsg);
            }

            Ok(if options.flags.contains(RecvFlags::TRUNCATE) {
                data.len()
            } else {
                count
            })
        })
    }
}

impl Pollable for DgramTransport {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::OUT;
        if let Some((rx, _)) = self.data_rx.lock().as_ref() {
            events.set(IoEvents::IN, !rx.is_empty());
        }
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        let poll = if let Some((_, poll)) = self.data_rx.lock().as_ref()
            && events.contains(IoEvents::IN)
        {
            Some(poll.clone())
        } else {
            None
        };
        if let Some(poll) = poll {
            // Registration happens from socket poll task context.
            unsafe { poll.register(context.waker(), IoEvents::IN) };
        }
    }
}

impl Drop for DgramTransport {
    fn drop(&mut self) {
        if let Some(chan) = self.connected.write().take() {
            // Connection teardown is visible before waking the peer.
            unsafe { chan.poll_update.wake(IoEvents::IN | IoEvents::OUT) };
        }
    }
}
