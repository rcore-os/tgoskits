//! Unix stream transport.
//!
//! Stream sockets are implemented as paired byte rings with explicit close
//! flags and a small cmsg side channel. Listening sockets enqueue connection
//! requests in the Unix namespace, and accepted sockets receive one half of a
//! connected channel pair.
//!
//! # Channel Layout
//!
//! A connected pair is two unidirectional byte rings plus shared close flags.
//! Each endpoint writes into one ring and reads from the other. This mirrors the
//! full-duplex behavior of Unix stream sockets without involving smoltcp.
//!
//! # Ancillary Data
//!
//! cmsg data is attached to byte ranges rather than individual bytes. The
//! receiver delivers a cmsg when it reaches the first byte of the send call that
//! carried it, and recv may stop at a cmsg boundary so the next recvmsg starts
//! with the next message's ancillary data.

use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use async_trait::async_trait;
use ax_errno::{AxError, AxResult};
use ax_io::{IoBuf, Read, Write};
use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet, Pollable};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};

use crate::{
    CMsgData, RecvOptions, SendOptions, Shutdown,
    general::GeneralOptions,
    options::{Configurable, GetSocketOption, SetSocketOption, UnixCredentials},
    unix::{Transport, TransportOps, UnixSocketAddr},
};

const BUF_SIZE: usize = 64 * 1024;

/// One pending cmsg batch carried across a Unix stream socketpair.
///
/// `start_byte` is the 1-based cumulative tx-byte offset of the first
/// byte of the send that carried this cmsg.  `end_byte` is the (1-based
/// inclusive) offset of the last byte of that same send.  These bound
/// the "message" that the cmsg belongs to.
///
/// On the recv side, `start_byte` is used to release the cmsg once the
/// consumer has read at least `start_byte` bytes (Linux's "cmsg
/// delivered with the first byte of its message").  `end_byte` caps a
/// recv at the end of the current cmsg-bearing message so the next
/// recvmsg starts cleanly at the next message.
struct PendingCmsg {
    start_byte: u64,
    end_byte: u64,
    cmsg: Vec<CMsgData>,
}

type CmsgQueue = Arc<Mutex<VecDeque<PendingCmsg>>>;

fn new_uni_channel() -> (HeapProd<u8>, HeapCons<u8>) {
    let rb = HeapRb::new(BUF_SIZE);
    rb.split()
}
fn new_channels(pid: u32) -> (Channel, Channel) {
    let (client_tx, server_rx) = new_uni_channel();
    let (server_tx, client_rx) = new_uni_channel();
    let poll_update = Arc::new(PollSet::new());
    let c2s_cmsg = CmsgQueue::default();
    let s2c_cmsg = CmsgQueue::default();
    // Cross-wired close flags: each side's my_tx_closed is the other's peer_tx_closed.
    let client_tx_closed = Arc::new(AtomicBool::new(false));
    let server_tx_closed = Arc::new(AtomicBool::new(false));
    (
        Channel {
            tx: client_tx,
            rx: client_rx,
            tx_cmsg: c2s_cmsg.clone(),
            rx_cmsg: s2c_cmsg.clone(),
            tx_bytes_total: 0,
            rx_bytes_total: 0,
            my_tx_closed: client_tx_closed.clone(),
            peer_tx_closed: server_tx_closed.clone(),
            poll_update: poll_update.clone(),
            peer_pid: pid,
        },
        Channel {
            tx: server_tx,
            rx: server_rx,
            tx_cmsg: s2c_cmsg,
            rx_cmsg: c2s_cmsg,
            tx_bytes_total: 0,
            rx_bytes_total: 0,
            my_tx_closed: server_tx_closed,
            peer_tx_closed: client_tx_closed,
            poll_update,
            peer_pid: pid,
        },
    )
}

struct Channel {
    tx: HeapProd<u8>,
    rx: HeapCons<u8>,
    /// Cmsg queue for the outgoing direction. On sendmsg we push a
    /// `PendingCmsg` covering the byte range of the call. The peer's
    /// recvmsg drains entries whose `start_byte` has been consumed.
    tx_cmsg: CmsgQueue,
    /// Cmsg queue for the incoming direction. Entries with
    /// `start_byte <= rx_bytes_total` are ready to deliver.
    rx_cmsg: CmsgQueue,
    /// Cumulative byte counter for the tx direction.
    tx_bytes_total: u64,
    /// Cumulative byte counter for the rx direction.
    rx_bytes_total: u64,
    /// Set to true by our Drop before waking the peer.
    my_tx_closed: Arc<AtomicBool>,
    /// Set to true by the peer's Drop before it wakes us.
    peer_tx_closed: Arc<AtomicBool>,
    poll_update: Arc<PollSet>,
    peer_pid: u32,
}

pub struct Bind {
    /// New connections are sent to this channel.
    conn_tx: async_channel::Sender<ConnRequest>,
    poll_new_conn: Arc<PollSet>,
    /// PID of the process that created the listening transport.
    pid: u32,
}
impl Bind {
    fn connect(&self, local_addr: UnixSocketAddr, pid: u32) -> AxResult<Channel> {
        let (mut client_chan, mut server_chan) = new_channels(0);
        client_chan.peer_pid = self.pid;
        server_chan.peer_pid = pid;
        self.conn_tx
            .try_send(ConnRequest {
                channel: server_chan,
                addr: local_addr,
                pid,
            })
            .map_err(|_| AxError::ConnectionRefused)?;
        self.poll_new_conn.wake();
        Ok(client_chan)
    }
}

struct ConnRequest {
    /// Server-side channel half created for accept().
    channel: Channel,
    /// Client address reported to accept().
    addr: UnixSocketAddr,
    /// Client pid used for peer credentials.
    pid: u32,
}

/// Stream transport for Unix domain sockets.
pub struct StreamTransport {
    /// Connected channel, if this endpoint is connected or accepted.
    channel: Mutex<Option<Channel>>,
    /// Listener receive queue installed by bind/listen.
    conn_rx: Mutex<Option<(async_channel::Receiver<ConnRequest>, Arc<PollSet>)>>,
    /// Poll set for local stream state.
    poll_state: PollSet,
    /// Shared socket options.
    general: GeneralOptions,
    /// Creator pid used for credentials.
    pid: u32,
    /// Public receive-half shutdown flag.
    rx_closed: AtomicBool,
    /// Public transmit-half shutdown flag.
    tx_closed: AtomicBool,
}
impl StreamTransport {
    /// Create a new unconnected stream transport.
    pub fn new(pid: u32) -> Self {
        StreamTransport::new_channel(None, pid)
    }

    fn new_channel(channel: Option<Channel>, pid: u32) -> Self {
        StreamTransport {
            channel: Mutex::new(channel),
            conn_rx: Mutex::new(None),
            poll_state: PollSet::new(),
            general: GeneralOptions::new(1, 1, 0), // SOCK_STREAM
            pid,
            rx_closed: AtomicBool::new(false),
            tx_closed: AtomicBool::new(false),
        }
    }

    /// Create a connected pair of stream transports.
    pub fn new_pair(pid: u32) -> (Self, Self) {
        let (chan1, chan2) = new_channels(pid);
        let transport1 = StreamTransport::new_channel(Some(chan1), pid);
        let transport2 = StreamTransport::new_channel(Some(chan2), pid);
        (transport1, transport2)
    }
}

impl Configurable for StreamTransport {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> AxResult<bool> {
        use GetSocketOption as O;

        if self.general.get_option_inner(opt)? {
            return Ok(true);
        }

        match opt {
            O::SendBuffer(size) => {
                **size = BUF_SIZE;
            }
            O::PassCredentials(_) => {}
            O::PeerCredentials(cred) => {
                let peer_pid = self
                    .channel
                    .lock()
                    .as_ref()
                    .map_or(self.pid, |chan| chan.peer_pid);
                **cred = UnixCredentials::new(peer_pid);
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
impl TransportOps for StreamTransport {
    fn bind(&self, slot: &super::BindSlot, _local_addr: &UnixSocketAddr) -> AxResult<()> {
        let mut slot = slot.stream.lock();
        if slot.is_some() {
            return Err(AxError::AddrInUse);
        }
        let mut guard = self.conn_rx.lock();
        if guard.is_some() {
            return Err(AxError::InvalidInput);
        }
        let (tx, rx) = async_channel::unbounded();
        let poll = Arc::new(PollSet::new());
        *slot = Some(Bind {
            conn_tx: tx,
            poll_new_conn: poll.clone(),
            pid: self.pid,
        });
        *guard = Some((rx, poll));
        self.poll_state.wake();
        Ok(())
    }

    fn connect(&self, slot: &super::BindSlot, local_addr: &UnixSocketAddr) -> AxResult<()> {
        let mut guard = self.channel.lock();
        if guard.is_some() {
            return Err(AxError::AlreadyConnected);
        }
        *guard = Some(
            slot.stream
                .lock()
                .as_ref()
                .ok_or(AxError::NotConnected)?
                .connect(local_addr.clone(), self.pid)?,
        );
        self.poll_state.wake();
        Ok(())
    }

    async fn accept(&self) -> AxResult<(Transport, UnixSocketAddr)> {
        let Some((rx, _)) = self.conn_rx.lock().clone() else {
            return Err(AxError::NotConnected);
        };
        let ConnRequest {
            channel,
            addr: peer_addr,
            pid,
        } = rx.recv().await.map_err(|_| AxError::ConnectionReset)?;
        Ok((
            Transport::Stream(StreamTransport::new_channel(Some(channel), pid)),
            peer_addr,
        ))
    }

    fn try_accept(&self) -> AxResult<(Transport, UnixSocketAddr)> {
        let Some((rx, _)) = self.conn_rx.lock().clone() else {
            return Err(AxError::NotConnected);
        };
        match rx.try_recv() {
            Ok(ConnRequest {
                channel,
                addr: peer_addr,
                pid,
            }) => Ok((
                Transport::Stream(StreamTransport::new_channel(Some(channel), pid)),
                peer_addr,
            )),
            Err(async_channel::TryRecvError::Empty) => Err(AxError::WouldBlock),
            Err(async_channel::TryRecvError::Closed) => Err(AxError::ConnectionReset),
        }
    }

    fn send(&self, mut src: impl Read + IoBuf, mut options: SendOptions) -> AxResult<usize> {
        if options.to.is_some() {
            return Err(AxError::InvalidInput);
        }
        let size = src.remaining();
        let mut total = 0;
        let dontwait = options.flags.contains(crate::SendFlags::DONTWAIT);
        let non_blocking = self.general.nonblocking() || dontwait;
        // Attach any incoming cmsg to the first byte written on this send
        // call (Linux semantics: cmsg is delivered with the first byte of
        // the message). We stash the vec here and push into the peer's
        // cmsg queue once some bytes actually got written.
        let pending_cmsg = core::mem::take(&mut options.cmsg);
        let had_cmsg = !pending_cmsg.is_empty();
        let mut cmsg_slot: Option<Vec<CMsgData>> = had_cmsg.then_some(pending_cmsg);

        self.general.send_poller_with(self, dontwait, || {
            let mut guard = self.channel.lock();
            let Some(chan) = guard.as_mut() else {
                return Err(AxError::NotConnected);
            };
            if !chan.tx.read_is_held() {
                return Err(AxError::BrokenPipe);
            }

            let count = {
                let (left, right) = chan.tx.vacant_slices_mut();
                let mut count = src.read(unsafe { left.assume_init_mut() })?;
                if count >= left.len() {
                    count += src.read(unsafe { right.assume_init_mut() })?;
                }
                unsafe { chan.tx.advance_write_index(count) };
                count
            };
            total += count;
            if count > 0 {
                // Attach cmsg (if any) to the first write of this send
                // call.  Continuations of the same multi-iter send extend
                // the just-pushed entry; back-to-back separate send calls
                // (no cmsg) must NOT extend a prior call's cmsg, otherwise
                // a non-cmsg send glued to a preceding cmsg send would
                // appear to peer as a single oversized cmsg-bearing
                // message.
                if let Some(cmsg) = cmsg_slot.take() {
                    let start_byte = chan.tx_bytes_total.saturating_add(1);
                    let end_byte = chan.tx_bytes_total.saturating_add(count as u64);
                    chan.tx_cmsg.lock().push_back(PendingCmsg {
                        start_byte,
                        end_byte,
                        cmsg,
                    });
                } else if had_cmsg
                    && let Some(last) = chan.tx_cmsg.lock().back_mut()
                    && last.end_byte == chan.tx_bytes_total
                {
                    last.end_byte = last.end_byte.saturating_add(count as u64);
                }
                chan.tx_bytes_total = chan.tx_bytes_total.saturating_add(count as u64);
                chan.poll_update.wake();
            }

            if count == size || non_blocking {
                Ok(total)
            } else {
                Err(AxError::WouldBlock)
            }
        })
    }

    fn recv(&self, mut dst: impl Write, mut options: RecvOptions) -> AxResult<usize> {
        let dontwait = options.flags.contains(crate::RecvFlags::DONTWAIT);
        let peek = options.flags.contains(crate::RecvFlags::PEEK);
        let recv_count = self.general.recv_poller_with(self, dontwait, || {
            let mut guard = self.channel.lock();
            let Some(chan) = guard.as_mut() else {
                return Err(AxError::NotConnected);
            };

            // Cap the read at the end of the first pending cmsg-bearing
            // message so the next recv starts cleanly at the next message.
            let cap_bytes: Option<usize> = {
                let q = chan.rx_cmsg.lock();
                q.front().and_then(|front| {
                    if front.end_byte > chan.rx_bytes_total {
                        let cap = front.end_byte.saturating_sub(chan.rx_bytes_total);
                        Some(cap as usize)
                    } else {
                        None
                    }
                })
            };

            let count = {
                let (left, right) = chan.rx.as_slices();
                let left_cap = cap_bytes.map_or(left.len(), |c| c.min(left.len()));
                let mut count = dst.write(&left[..left_cap])?;
                let remaining_cap = cap_bytes.map_or(usize::MAX, |c| c.saturating_sub(count));
                if count >= left_cap && remaining_cap > 0 {
                    let right_cap = right.len().min(remaining_cap);
                    count += dst.write(&right[..right_cap])?;
                }
                if !peek {
                    unsafe { chan.rx.advance_read_index(count) };
                }
                count
            };
            if count > 0 {
                if !peek {
                    chan.rx_bytes_total = chan.rx_bytes_total.saturating_add(count as u64);
                    chan.poll_update.wake();
                }
                Ok(count)
            } else if !chan.rx.write_is_held() || chan.peer_tx_closed.load(Ordering::Acquire) {
                // Peer closed (HeapProd dropped or tx_closed flag set): EOF.
                Ok(0)
            } else {
                Err(AxError::WouldBlock)
            }
        })?;

        if peek {
            // MSG_PEEK must not advance the cmsg byte-mark queue. Ancillary
            // data is attached to the first byte of the carrying message;
            // delivering and popping it on PEEK would consume the cmsg and
            // (for SCM_RIGHTS) duplicate file descriptors. A later non-PEEK
            // recv that actually consumes those bytes will deliver the cmsg.
            return Ok(recv_count);
        }

        // Drain every cmsg whose attached message's first byte has been
        // consumed by this recv. Linux's man recvmsg(2) is explicit:
        // ancillary data is delivered to the receiver only on the call
        // that reads the first byte. A recv that consumes the first
        // byte without an msg_control buffer must still discard the
        // pending cmsg, otherwise a later recvmsg that does pass a
        // control buffer would silently inherit stale ancillary data.
        // The read cap above stops at the boundary of the *next*
        // cmsg-bearing message, so at most one entry becomes ready
        // per call.
        let mut dst_cmsg = options.cmsg.as_deref_mut();
        let mut guard = self.channel.lock();
        if let Some(chan) = guard.as_mut() {
            let mut q = chan.rx_cmsg.lock();
            while let Some(front) = q.front()
                && front.start_byte <= chan.rx_bytes_total
            {
                let entry = q.pop_front().unwrap();
                if let Some(dst) = dst_cmsg.as_deref_mut() {
                    dst.extend(entry.cmsg);
                }
            }
        }

        Ok(recv_count)
    }

    fn shutdown(&self, how: Shutdown) -> AxResult<()> {
        if how.has_read() {
            self.rx_closed.store(true, Ordering::Release);
            self.poll_state.wake();
        }
        if how.has_write() {
            self.tx_closed.store(true, Ordering::Release);
            self.poll_state.wake();
        }
        if self.rx_closed.load(Ordering::Acquire)
            && self.tx_closed.load(Ordering::Acquire)
            && let Some(chan) = self.channel.lock().take()
        {
            chan.poll_update.wake();
        }
        Ok(())
    }
}

impl Pollable for StreamTransport {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        let rx_closed = self.rx_closed.load(Ordering::Acquire);
        let mut peer_eof = false;
        if let Some(chan) = self.channel.lock().as_ref() {
            peer_eof = chan.peer_tx_closed.load(Ordering::Acquire);
            // Report IN when data is available OR when peer has closed (EOF to drain).
            events.set(
                IoEvents::IN,
                !rx_closed && (chan.rx.occupied_len() > 0 || peer_eof),
            );
            events.set(
                IoEvents::OUT,
                !self.tx_closed.load(Ordering::Acquire) && chan.tx.vacant_len() > 0,
            );
        } else if let Some((conn_tx, _)) = self.conn_rx.lock().as_ref() {
            events.set(IoEvents::IN, !conn_tx.is_empty());
        }
        events.set(IoEvents::RDHUP, peer_eof || rx_closed);
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if let Some(chan) = self.channel.lock().as_ref() {
            if events.intersects(IoEvents::IN | IoEvents::OUT) {
                chan.poll_update.register(context.waker());
            }
        } else if let Some((_, poll_new_conn)) = self.conn_rx.lock().as_ref()
            && events.contains(IoEvents::IN)
        {
            poll_new_conn.register(context.waker());
        }
        self.poll_state.register(context.waker());
    }
}

impl Drop for StreamTransport {
    fn drop(&mut self) {
        if let Some(chan) = self.channel.lock().as_ref() {
            // Set the flag BEFORE waking the peer so poll() sees peer_eof=true
            // when it runs in the wake handler — even though our HeapProd hasn't
            // dropped yet.  Without this, the peer's poll() sees write_is_held()=true
            // and no data, reports no events, and parks forever waiting for data
            // that will never arrive.
            chan.my_tx_closed.store(true, Ordering::Release);
            chan.poll_update.wake();
        }
        self.poll_state.wake();
    }
}
