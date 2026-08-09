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
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
    time::Duration,
};

use async_channel::TryRecvError;
use async_trait::async_trait;
use ax_errno::{AxError, AxResult};
use ax_hal::time::wall_time;
use ax_io::{Read, Write};
use ax_kspin::SpinRwLock as RwLock;
use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet, Pollable};

use crate::{
    CMsgData, RecvFlags, RecvOptions, SendOptions, SocketAddrEx, SocketCmsg,
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
    /// Wall-clock time captured immediately before this packet entered the
    /// receiver queue while `SO_TIMESTAMP` was enabled.
    received_at: Option<Duration>,
}

/// Receiver plus its poll set: the half a socket reads incoming packets from.
type PacketRx = (async_channel::Receiver<Packet>, Arc<PollSet>);

struct Channel {
    /// Sender side of the peer's datagram queue.
    data_tx: async_channel::Sender<Packet>,
    /// Poll set woken when data is queued.
    poll_update: Arc<PollSet>,
    /// Target receiver's `SO_TIMESTAMP` state.
    receive_timestamp: Arc<AtomicBool>,
    /// Target receiver's `SO_PASSCRED` state.
    receive_credentials: Arc<AtomicBool>,
}

pub struct Bind {
    /// Sender published in the Unix namespace for this bound address.
    data_tx: async_channel::Sender<Packet>,
    /// Poll set associated with the receiver bound at this address.
    poll_update: Arc<PollSet>,
    /// Bound receiver's `SO_TIMESTAMP` state.
    receive_timestamp: Arc<AtomicBool>,
    /// Bound receiver's `SO_PASSCRED` state.
    receive_credentials: Arc<AtomicBool>,
}
impl Bind {
    fn connect(&self) -> Channel {
        let tx = self.data_tx.clone();
        Channel {
            data_tx: tx,
            poll_update: self.poll_update.clone(),
            receive_timestamp: self.receive_timestamp.clone(),
            receive_credentials: self.receive_credentials.clone(),
        }
    }
}

/// Server-side halves handed to a seqpacket listener's `accept`.
struct SeqConnRequest {
    /// Receiver + poll set the accepted socket reads incoming packets from.
    data_rx: PacketRx,
    /// Channel the accepted socket sends packets to the client through.
    connected: Channel,
    /// Client address reported to `accept`.
    addr: UnixSocketAddr,
    /// Client pid used for peer credentials.
    pid: u32,
    /// Timestamp state owned by the accepted server socket.
    receive_timestamp: Arc<AtomicBool>,
    /// Passcred state owned by the accepted server socket.
    receive_credentials: Arc<AtomicBool>,
}

/// Seqpacket listener published in the Unix namespace.
///
/// Seqpacket is connection-oriented like stream but preserves message
/// boundaries like dgram, so `connect`/`accept` exchange packet channels
/// through this queue (mirroring `stream::Bind`), matching Linux where
/// `unix_seqpacket_ops` reuses `unix_stream_connect` / `unix_accept`.
pub struct SeqBind {
    conn_tx: async_channel::Sender<SeqConnRequest>,
    poll_new_conn: Arc<PollSet>,
    listening: Arc<AtomicBool>,
    /// Passcred state inherited by accepted transports.
    receive_credentials: Arc<AtomicBool>,
}
impl SeqBind {
    /// Establish a connection: build a packet channel pair, hand the
    /// server side to the listener, and return the client side.
    fn connect(
        &self,
        addr: UnixSocketAddr,
        pid: u32,
        client_receive_timestamp: Arc<AtomicBool>,
        client_receive_credentials: Arc<AtomicBool>,
    ) -> AxResult<(PacketRx, Channel)> {
        if !self.listening.load(Ordering::Acquire) {
            return Err(AxError::ConnectionRefused);
        }
        let (tx1, rx1) = async_channel::unbounded();
        let (tx2, rx2) = async_channel::unbounded();
        let poll1 = Arc::new(PollSet::new());
        let poll2 = Arc::new(PollSet::new());
        let server_receive_timestamp = Arc::new(AtomicBool::new(false));
        let server_receive_credentials = Arc::new(AtomicBool::new(
            self.receive_credentials.load(Ordering::Acquire),
        ));
        self.conn_tx
            .try_send(SeqConnRequest {
                data_rx: (rx2, poll2.clone()),
                connected: Channel {
                    data_tx: tx1,
                    poll_update: poll1.clone(),
                    receive_timestamp: client_receive_timestamp,
                    receive_credentials: client_receive_credentials,
                },
                addr,
                pid,
                receive_timestamp: server_receive_timestamp.clone(),
                receive_credentials: server_receive_credentials.clone(),
            })
            .map_err(|_| AxError::ConnectionRefused)?;
        // The connection request is queued before waking accept waiters.
        unsafe { self.poll_new_conn.wake(IoEvents::IN) };
        Ok((
            (rx1, poll1),
            Channel {
                data_tx: tx2,
                poll_update: poll2,
                receive_timestamp: server_receive_timestamp,
                receive_credentials: server_receive_credentials,
            },
        ))
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
    /// Packet held back by a `MSG_PEEK` recv, consumed by the next recv.
    ///
    /// The async channel has no peek primitive, so a peeking receiver pops one
    /// packet, copies it out, and parks it here; the next recv drains this slot
    /// before touching the channel, preserving record boundaries and order.
    peeked: Mutex<Option<Packet>>,
    /// True for `SOCK_SEQPACKET`, which is connection-oriented (bind/listen/
    /// accept/connect) unlike connectionless `SOCK_DGRAM`.
    is_seqpacket: bool,
    /// Connection-request queue installed by a seqpacket listener's bind.
    conn_rx: Mutex<Option<(async_channel::Receiver<SeqConnRequest>, Arc<PollSet>)>>,
    /// True after a bound seqpacket socket enters listening state.
    listening: Arc<AtomicBool>,
    /// Poll set for local state changes.
    poll_state: Arc<PollSet>,
    /// Shared socket options.
    general: GeneralOptions,
    /// Per-receiver `SO_TIMESTAMP` state shared with channels targeting this
    /// socket.
    receive_timestamp: Arc<AtomicBool>,
    /// Per-receiver `SO_PASSCRED` state shared with channels targeting this
    /// socket.
    receive_credentials: Arc<AtomicBool>,
    /// Creator pid used for SO_PEERCRED-style reporting.
    pid: u32,
}
impl DgramTransport {
    /// Create a new unconnected `SOCK_DGRAM` transport.
    pub fn new(pid: u32) -> Self {
        Self::new_typed(pid, 2) // SOCK_DGRAM
    }

    /// Create a new unconnected `SOCK_SEQPACKET` transport.
    ///
    /// SEQPACKET reuses the datagram delivery path (message boundaries), but
    /// reports its own `SO_TYPE` and is connection-oriented at the syscall
    /// layer, matching `net/unix/af_unix.c` `unix_seqpacket_ops`.
    pub fn new_seqpacket(pid: u32) -> Self {
        Self::new_typed(pid, 5) // SOCK_SEQPACKET
    }

    fn new_typed(pid: u32, socket_type: i32) -> Self {
        DgramTransport {
            data_rx: Mutex::new(None),
            connected: RwLock::new(None),
            local_addr: RwLock::new(UnixSocketAddr::Unnamed),
            peeked: Mutex::new(None),
            is_seqpacket: socket_type == 5,
            conn_rx: Mutex::new(None),
            listening: Arc::new(AtomicBool::new(false)),
            poll_state: Arc::default(),
            general: GeneralOptions::new(socket_type, 1, 0),
            receive_timestamp: Arc::new(AtomicBool::new(false)),
            receive_credentials: Arc::new(AtomicBool::new(false)),
            pid,
        }
    }

    fn new_connected(
        data_rx: (async_channel::Receiver<Packet>, Arc<PollSet>),
        connected: Channel,
        pid: u32,
        socket_type: i32,
        receive_timestamp: Arc<AtomicBool>,
        receive_credentials: Arc<AtomicBool>,
    ) -> Self {
        DgramTransport {
            data_rx: Mutex::new(Some(data_rx)),
            connected: RwLock::new(Some(connected)),
            local_addr: RwLock::new(UnixSocketAddr::Unnamed),
            peeked: Mutex::new(None),
            is_seqpacket: socket_type == 5,
            conn_rx: Mutex::new(None),
            listening: Arc::new(AtomicBool::new(false)),
            poll_state: Arc::default(),
            general: GeneralOptions::new(socket_type, 1, 0),
            receive_timestamp,
            receive_credentials,
            pid,
        }
    }

    /// Create a connected pair of `SOCK_DGRAM` transports.
    pub fn new_pair(pid: u32) -> (Self, Self) {
        Self::new_pair_typed(pid, 2) // SOCK_DGRAM
    }

    /// Create a connected pair of `SOCK_SEQPACKET` transports.
    pub fn new_pair_seqpacket(pid: u32) -> (Self, Self) {
        Self::new_pair_typed(pid, 5) // SOCK_SEQPACKET
    }

    fn new_pair_typed(pid: u32, socket_type: i32) -> (Self, Self) {
        let (tx1, rx1) = async_channel::unbounded();
        let (tx2, rx2) = async_channel::unbounded();
        let poll1 = Arc::new(PollSet::new());
        let poll2 = Arc::new(PollSet::new());
        let timestamp1 = Arc::new(AtomicBool::new(false));
        let timestamp2 = Arc::new(AtomicBool::new(false));
        let credentials1 = Arc::new(AtomicBool::new(false));
        let credentials2 = Arc::new(AtomicBool::new(false));
        let transport1 = DgramTransport::new_connected(
            (rx1, poll1.clone()),
            Channel {
                data_tx: tx2,
                poll_update: poll2.clone(),
                receive_timestamp: timestamp2.clone(),
                receive_credentials: credentials2.clone(),
            },
            pid,
            socket_type,
            timestamp1.clone(),
            credentials1.clone(),
        );
        let transport2 = DgramTransport::new_connected(
            (rx2, poll2.clone()),
            Channel {
                data_tx: tx1,
                poll_update: poll1.clone(),
                receive_timestamp: timestamp1,
                receive_credentials: credentials1,
            },
            pid,
            socket_type,
            timestamp2,
            credentials2,
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
            O::PassCredentials(enabled) => {
                **enabled = self.receive_credentials.load(Ordering::Acquire);
            }
            O::ReceiveTimestamp(enabled) => {
                **enabled = self.receive_timestamp.load(Ordering::Acquire);
            }
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
            O::PassCredentials(enabled) => {
                self.receive_credentials.store(*enabled, Ordering::Release);
            }
            O::ReceiveTimestamp(enabled) => {
                self.receive_timestamp.store(*enabled, Ordering::Release);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
#[async_trait]
impl TransportOps for DgramTransport {
    fn bind(&self, slot: &super::BindSlot, local_addr: &UnixSocketAddr) -> AxResult {
        if self.is_seqpacket {
            // Seqpacket bind installs a connection-request queue (like stream).
            let mut slot = slot.seqpacket.lock();
            if slot.is_some() {
                return Err(AxError::AddrInUse);
            }
            let mut guard = self.conn_rx.lock();
            if guard.is_some() {
                return Err(AxError::InvalidInput);
            }
            let (tx, rx) = async_channel::unbounded();
            let poll = Arc::new(PollSet::new());
            *slot = Some(SeqBind {
                conn_tx: tx,
                poll_new_conn: poll.clone(),
                listening: self.listening.clone(),
                receive_credentials: self.receive_credentials.clone(),
            });
            *guard = Some((rx, poll));
            self.local_addr.write().clone_from(local_addr);
            drop(guard);
            drop(slot);
            unsafe { self.poll_state.wake(IoEvents::IN | IoEvents::OUT) };
            return Ok(());
        }
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
            receive_timestamp: self.receive_timestamp.clone(),
            receive_credentials: self.receive_credentials.clone(),
        });
        *guard = Some((rx, poll_update));
        self.local_addr.write().clone_from(local_addr);
        drop(guard);
        drop(slot);
        // Datagram bind state is published before waking pollers.
        unsafe { self.poll_state.wake(IoEvents::IN | IoEvents::OUT) };
        Ok(())
    }

    fn listen(&self) -> AxResult {
        if !self.is_seqpacket {
            return Err(AxError::OperationNotSupported);
        }
        if self.conn_rx.lock().is_none() {
            return Err(AxError::InvalidInput);
        }
        self.listening.store(true, Ordering::Release);
        Ok(())
    }

    fn is_listening(&self) -> bool {
        self.is_seqpacket && self.listening.load(Ordering::Acquire)
    }

    fn connect(&self, slot: &super::BindSlot, _local_addr: &UnixSocketAddr) -> AxResult {
        if self.is_seqpacket {
            // Seqpacket connect performs the stream-style handshake: exchange a
            // packet channel pair with the listener, keep the client half.
            if self.connected.read().is_some() {
                return Err(AxError::AlreadyConnected);
            }
            let client_addr = self.local_addr.read().clone();
            let (client_rx, client_chan) = slot
                .seqpacket
                .lock()
                .as_ref()
                .ok_or(AxError::ConnectionRefused)?
                .connect(
                    client_addr,
                    self.pid,
                    self.receive_timestamp.clone(),
                    self.receive_credentials.clone(),
                )?;
            *self.data_rx.lock() = Some(client_rx);
            *self.connected.write() = Some(client_chan);
            unsafe { self.poll_state.wake(IoEvents::IN | IoEvents::OUT) };
            return Ok(());
        }
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
        if !self.is_seqpacket {
            // Connectionless SOCK_DGRAM has no accept: Linux net/unix/af_unix.c
            // `unix_dgram_ops.accept = sock_no_accept` returns -EOPNOTSUPP.
            return Err(AxError::OperationNotSupported);
        }
        if !self.is_listening() {
            return Err(AxError::InvalidInput);
        }
        let Some((rx, _)) = self.conn_rx.lock().clone() else {
            // Not a listening seqpacket socket: accept requires listen(). Linux
            // returns EINVAL for accept on a non-listening socket.
            return Err(AxError::InvalidInput);
        };
        let req = rx.recv().await.map_err(|_| AxError::ConnectionReset)?;
        let transport = DgramTransport::new_connected(
            req.data_rx,
            req.connected,
            req.pid,
            5,
            req.receive_timestamp,
            req.receive_credentials,
        );
        Ok((Transport::Dgram(transport), req.addr))
    }

    fn try_accept(&self) -> AxResult<(Transport, UnixSocketAddr)> {
        if !self.is_seqpacket {
            // Connectionless SOCK_DGRAM has no accept: Linux net/unix/af_unix.c
            // `unix_dgram_ops.accept = sock_no_accept` returns -EOPNOTSUPP.
            // Must not return WouldBlock, or the accept poll loop hangs forever.
            return Err(AxError::OperationNotSupported);
        }
        if !self.is_listening() {
            return Err(AxError::InvalidInput);
        }
        let Some((rx, _)) = self.conn_rx.lock().clone() else {
            // Not a listening seqpacket socket: accept requires listen(). Linux
            // returns EINVAL for accept on a non-listening socket.
            return Err(AxError::InvalidInput);
        };
        match rx.try_recv() {
            Ok(req) => {
                let transport = DgramTransport::new_connected(
                    req.data_rx,
                    req.connected,
                    req.pid,
                    5,
                    req.receive_timestamp,
                    req.receive_credentials,
                );
                Ok((Transport::Dgram(transport), req.addr))
            }
            Err(TryRecvError::Empty) => Err(AxError::WouldBlock),
            Err(TryRecvError::Closed) => Err(AxError::ConnectionReset),
        }
    }

    fn send(&self, mut src: impl Read, options: SendOptions) -> AxResult<usize> {
        // Unix datagram/seqpacket sockets do not carry out-of-band data.
        // Linux `unix_dgram_sendmsg` rejects MSG_OOB with EOPNOTSUPP.
        if options.flags.contains(crate::SendFlags::OOB) {
            return Err(AxError::OperationNotSupported);
        }
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
        let sender = self.local_addr.read().clone();
        let mut cmsg = options.cmsg;
        let sender_credentials = options.sender_credentials;

        let wake_poll = if let Some(addr) = options.to {
            let addr = addr.into_unix()?;
            with_slot(&addr, |slot| {
                if let Some(bind) = slot.dgram.lock().as_ref() {
                    if bind.receive_credentials.load(Ordering::Acquire)
                        && let Some(credentials) = sender_credentials
                    {
                        cmsg.push(Box::new(SocketCmsg::Credentials(credentials)));
                    }
                    let packet = Packet {
                        data: message,
                        cmsg,
                        sender,
                        received_at: bind
                            .receive_timestamp
                            .load(Ordering::Acquire)
                            .then(wall_time),
                    };
                    bind.data_tx
                        .try_send(packet)
                        .map_err(|_| AxError::BrokenPipe)?;
                    Ok(bind.poll_update.clone())
                } else {
                    Err(AxError::NotConnected)
                }
            })?
        } else if let Some(chan) = self.connected.read().as_ref() {
            if chan.receive_credentials.load(Ordering::Acquire)
                && let Some(credentials) = sender_credentials
            {
                cmsg.push(Box::new(SocketCmsg::Credentials(credentials)));
            }
            let packet = Packet {
                data: message,
                cmsg,
                sender,
                received_at: chan
                    .receive_timestamp
                    .load(Ordering::Acquire)
                    .then(wall_time),
            };
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
        // Unix datagram/seqpacket sockets do not carry out-of-band data.
        // Linux `unix_dgram_recvmsg` rejects MSG_OOB with EOPNOTSUPP.
        if options.flags.contains(RecvFlags::OOB) {
            return Err(AxError::OperationNotSupported);
        }
        let extra_nb = options.flags.contains(RecvFlags::DONTWAIT);
        let peek = options.flags.contains(RecvFlags::PEEK);
        self.general.recv_poller_with(self, extra_nb, move || {
            // Drain a packet parked by a previous MSG_PEEK before the channel,
            // preserving record order.
            let mut peeked = self.peeked.lock();
            let mut packet = if let Some(p) = peeked.take() {
                p
            } else {
                let mut guard = self.data_rx.lock();
                let Some((rx, _)) = guard.as_mut() else {
                    return Err(AxError::NotConnected);
                };
                match rx.try_recv() {
                    Ok(packet) => packet,
                    Err(TryRecvError::Empty) => return Err(AxError::WouldBlock),
                    Err(TryRecvError::Closed) => return Ok(0),
                }
            };

            let count = dst.write(&packet.data)?;
            let full_len = packet.data.len();
            // Surface truncation in the returned `msg_flags` (MSG_TRUNC).
            if count < full_len
                && let Some(t) = options.truncated.as_mut()
            {
                **t = true;
            }
            if let Some(from) = options.from.as_mut() {
                **from = SocketAddrEx::Unix(packet.sender.clone());
            }
            let receive_timestamp = self.receive_timestamp.load(Ordering::Acquire);
            if receive_timestamp && packet.received_at.is_none() {
                // Linux fills the current time when SO_TIMESTAMP was enabled
                // after this datagram entered the receive queue. Persist the
                // fallback on the packet so MSG_PEEK and the consuming recv
                // observe the same timestamp.
                packet.received_at = Some(wall_time());
            }
            if peek {
                // MSG_PEEK does not consume the record: deliver a duplicate of
                // the ancillary data (SCM_RIGHTS fds are cloned via Arc, sharing
                // the open file description like Linux `unix_peek_fds` /
                // `scm_fp_dup`) and re-park the packet so the next recv delivers
                // the rights again.
                if let Some(dst) = options.cmsg.as_mut() {
                    dst.extend(packet.cmsg.iter().map(|c| c.clone_box()));
                    if receive_timestamp && let Some(timestamp) = packet.received_at {
                        dst.push(Box::new(SocketCmsg::Timestamp(timestamp)));
                    }
                }
                *peeked = Some(packet);
            } else if let Some(dst) = options.cmsg.as_mut() {
                dst.extend(packet.cmsg);
                if receive_timestamp && let Some(timestamp) = packet.received_at {
                    dst.push(Box::new(SocketCmsg::Timestamp(timestamp)));
                }
            }

            Ok(if options.flags.contains(RecvFlags::TRUNCATE) {
                full_len
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
        // A packet parked by MSG_PEEK is immediately readable.
        if self.peeked.lock().is_some() {
            events.insert(IoEvents::IN);
        }
        // Seqpacket listener: readable when a connection is pending.
        if let Some((rx, _)) = self.conn_rx.lock().as_ref()
            && !rx.is_empty()
        {
            events.insert(IoEvents::IN);
        }
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if !events.contains(IoEvents::IN) {
            return;
        }
        // Registration happens from socket poll task context.
        if let Some((_, poll)) = self.data_rx.lock().as_ref() {
            unsafe { poll.register(context.waker(), IoEvents::IN) };
        }
        // Seqpacket listener waits for incoming connections.
        if let Some((_, poll)) = self.conn_rx.lock().as_ref() {
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
