#![no_std]

//! Peer-addressed, allocation-free messages in AxVisor ivshmem output sections.
//!
//! This crate does not map PCI BARs, write BAR0 State, or ring doorbells. The
//! guest adapter supplies the State Table and contiguous output sections, and
//! publishes READY only after [`Peer::initialize`] succeeds. A single owner
//! drives each peer's sender and receiver; other peers only read its section.
//! The [`message`] module streams Message V1 frames across these slots.
//! Guest PCI/device integration remains in the OS adapter.

use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(test)]
extern crate std;

pub mod message;

/// Bytes in one output section (one BAR2 page).
pub const SECTION_SIZE: usize = 4096;
/// Number of data slots per section.
pub const CAPACITY: usize = 15;
/// Maximum number of peers supported by the metadata slot.
pub const MAX_PEERS: usize = 15;
/// Maximum payload bytes in one data slot.
pub const PAYLOAD_SIZE: usize = 240;
/// Destination ID for a message addressed to all ready peers.
pub const BROADCAST: u16 = 0xffff;
/// Value the guest writes to BAR0 State after initializing its section.
pub const READY: u32 = 1;
const MAGIC: u32 = 0x4956_4333; // "IVC3"
const VERSION: u32 = 1;
// 0xffff_ffff is reserved: the sequence cycle is divisible by CAPACITY.
const MODULUS: u32 = u32::MAX;

/// A single peer-owned 4 KiB output section. The owner writes it; all other
/// peers map it read-only. The underlying memory must be coherent across VMs.
#[repr(C, align(256))]
pub struct Section {
    header: SectionHeader,
    slots: [Slot; CAPACITY],
}

#[repr(C, align(256))]
struct SectionHeader {
    magic: AtomicU32,
    version: AtomicU32,
    capacity: AtomicU32,
    slot_size: AtomicU32,
    tail: AtomicU32,
    credit: [AtomicU32; MAX_PEERS],
    reserved: [AtomicU32; 44],
}

#[repr(C, align(256))]
struct Slot {
    seq: AtomicU32,
    dst: AtomicU32,
    len: AtomicU32,
    flags: AtomicU32,
    payload: [AtomicU32; PAYLOAD_SIZE / 4],
}

impl Section {
    /// Constructs an uninitialized protocol section (for owned backing memory).
    /// A mapped section may instead be viewed through the adapter's mapping.
    pub const fn new() -> Self {
        Self {
            header: SectionHeader {
                magic: AtomicU32::new(0),
                version: AtomicU32::new(0),
                capacity: AtomicU32::new(0),
                slot_size: AtomicU32::new(0),
                tail: AtomicU32::new(0),
                credit: [const { AtomicU32::new(0) }; MAX_PEERS],
                reserved: [const { AtomicU32::new(0) }; 44],
            },
            slots: [const {
                Slot {
                    seq: AtomicU32::new(MODULUS),
                    dst: AtomicU32::new(0),
                    len: AtomicU32::new(0),
                    flags: AtomicU32::new(0),
                    payload: [const { AtomicU32::new(0) }; PAYLOAD_SIZE / 4],
                }
            }; CAPACITY],
        }
    }

    fn matches(&self) -> bool {
        self.header.magic.load(Ordering::Acquire) == MAGIC
            && self.header.version.load(Ordering::Relaxed) == VERSION
            && self.header.capacity.load(Ordering::Relaxed) == CAPACITY as u32
            && self.header.slot_size.load(Ordering::Relaxed) == 256
    }
}

impl Default for Section {
    fn default() -> Self {
        Self::new()
    }
}

/// Protocol failure. `Full` identifies a lagging READY peer to nudge with a
/// doorbell; all other failures require rechecking the link or its mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidProfile,
    NotReady,
    InvalidDestination,
    PayloadTooLarge,
    Full { lagging_peer: u16 },
    IncompatibleSection { peer: u16 },
    InvalidSequence { peer: u16 },
    InvalidSlot { peer: u16 },
}

/// A copied message. Its payload is in the caller's output buffer and remains
/// valid after the corresponding credit has been released.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Message {
    pub source: u16,
    pub destination: u16,
    pub len: usize,
}

/// One peer's exclusive protocol endpoint. Use a single owner/task (or an
/// external lock) for both sending and sweeping received sections.
pub struct Peer<'a> {
    id: usize,
    sections: &'a [Section],
    states: &'a [AtomicU32],
    initialized: bool,
}

impl<'a> Peer<'a> {
    /// Attaches one endpoint to the State Table and contiguous output pages.
    /// `sections[i]` must refer to the output page owned by peer `i`.
    ///
    /// # Safety
    ///
    /// The mapped ranges must remain valid, aligned and cache-coherent for
    /// `'a`. They must contain initialized atomic storage (zero-filled BAR2 is
    /// sufficient). Exactly one endpoint may write the local output section;
    /// foreign sections must not be writable by this peer. No section may be
    /// reset while another endpoint can still access its old generation.
    pub unsafe fn attach(
        id: usize,
        sections: &'a [Section],
        states: &'a [AtomicU32],
    ) -> Result<Self, Error> {
        if sections.is_empty()
            || sections.len() > MAX_PEERS
            || states.len() != sections.len()
            || id >= sections.len()
        {
            return Err(Error::InvalidProfile);
        }
        Ok(Self {
            id,
            sections,
            states,
            initialized: false,
        })
    }

    /// Initializes only this peer's output page. Call with BAR0 State cleared;
    /// after this returns, publish READY through the device's BAR0 State register.
    /// A new peer starts at the current tail of every READY publisher. The
    /// adapter must quiesce publishers while a peer joins: without a join
    /// handshake, a publisher not yet seeing READY could lap the new peer
    /// between its tail snapshot and the State write. Reinitialization also
    /// requires all users of the old link generation to have stopped.
    pub fn initialize(&mut self) -> Result<(), Error> {
        if self.states[self.id].load(Ordering::Acquire) != 0 {
            return Err(Error::NotReady);
        }
        let local = &self.sections[self.id];
        local.header.magic.store(0, Ordering::Release);
        self.initialized = false;
        for (index, section) in self.sections.iter().enumerate() {
            let cursor = if index != self.id && self.is_ready(index) {
                self.check_section(index)?;
                let tail = section.header.tail.load(Ordering::Acquire);
                if tail == MODULUS {
                    return Err(Error::InvalidSequence { peer: index as u16 });
                }
                tail
            } else {
                0
            };
            local.header.credit[index].store(cursor, Ordering::Relaxed);
        }
        for slot in &local.slots {
            slot.seq.store(MODULUS, Ordering::Relaxed);
        }
        local.header.tail.store(0, Ordering::Relaxed);
        local.header.version.store(VERSION, Ordering::Relaxed);
        local
            .header
            .capacity
            .store(CAPACITY as u32, Ordering::Relaxed);
        local.header.slot_size.store(256, Ordering::Relaxed);
        local.header.magic.store(MAGIC, Ordering::Release);
        self.initialized = true;
        Ok(())
    }

    /// Publishes one message; the caller may then send a directed doorbell
    /// (or rely on polling). Does not overwrite unconsumed slots. Broadcast
    /// recipients must be notified individually by the guest adapter.
    pub fn try_send(&mut self, destination: u16, payload: &[u8]) -> Result<(), Error> {
        self.ensure_ready()?;
        if payload.len() > PAYLOAD_SIZE {
            return Err(Error::PayloadTooLarge);
        }
        self.validate_destination(destination)?;
        let local = &self.sections[self.id];
        let tail = local.header.tail.load(Ordering::Relaxed);
        if tail == MODULUS {
            return Err(Error::InvalidSequence {
                peer: self.id as u16,
            });
        }
        for index in 0..self.sections.len() {
            if index == self.id || !self.is_ready(index) {
                continue;
            }
            self.check_section(index)?;
            let head = self.sections[index].header.credit[self.id].load(Ordering::Acquire);
            if head == MODULUS || seq_distance(head, tail) > CAPACITY as u32 {
                return Err(Error::InvalidSequence { peer: index as u16 });
            }
            if seq_distance(head, tail) == CAPACITY as u32 {
                return Err(Error::Full {
                    lagging_peer: index as u16,
                });
            }
        }
        let slot = &local.slots[tail as usize % CAPACITY];
        for (word, chunk) in slot.payload.iter().zip(payload.chunks(4)) {
            let mut bytes = [0; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            word.store(u32::from_le_bytes(bytes), Ordering::Relaxed);
        }
        slot.dst.store(destination as u32, Ordering::Relaxed);
        slot.len.store(payload.len() as u32, Ordering::Relaxed);
        slot.flags.store(0, Ordering::Relaxed);
        slot.seq.store(tail, Ordering::Release);
        local.header.tail.store(seq_next(tail), Ordering::Release);
        Ok(())
    }

    /// Sweeps one publisher until a message addressed to this peer is copied
    /// or its acquired tail is exhausted. Nonmatching slots still advance
    /// credit. Sweep *every* publisher after waking, including idle sources.
    /// The caller must handle the returned payload before reusing `output`.
    pub fn try_receive(
        &mut self,
        publisher: u16,
        output: &mut [u8; PAYLOAD_SIZE],
    ) -> Result<Option<Message>, Error> {
        self.ensure_ready()?;
        let index = publisher as usize;
        if index >= self.sections.len() || index == self.id {
            return Err(Error::InvalidDestination);
        }
        if !self.is_ready(index) {
            return Ok(None);
        }
        self.check_section(index)?;
        let remote = &self.sections[index];
        let credit = &self.sections[self.id].header.credit[index];
        let tail = remote.header.tail.load(Ordering::Acquire);
        let mut cursor = credit.load(Ordering::Relaxed);
        if tail == MODULUS || cursor == MODULUS || seq_distance(cursor, tail) > CAPACITY as u32 {
            return Err(Error::InvalidSequence { peer: publisher });
        }
        while cursor != tail {
            let slot = &remote.slots[cursor as usize % CAPACITY];
            if slot.seq.load(Ordering::Acquire) != cursor {
                return Err(Error::InvalidSequence { peer: publisher });
            }
            let destination = slot.dst.load(Ordering::Relaxed);
            let len = slot.len.load(Ordering::Relaxed) as usize;
            let flags = slot.flags.load(Ordering::Relaxed);
            if len > PAYLOAD_SIZE || flags != 0 || destination > u16::MAX as u32 {
                return Err(Error::InvalidSlot { peer: publisher });
            }
            if destination == self.id as u32 || destination == BROADCAST as u32 {
                for (chunk, word) in output[..len].chunks_mut(4).zip(&slot.payload) {
                    chunk.copy_from_slice(
                        &word.load(Ordering::Relaxed).to_le_bytes()[..chunk.len()],
                    );
                }
                credit.store(seq_next(cursor), Ordering::Release);
                return Ok(Some(Message {
                    source: publisher,
                    destination: destination as u16,
                    len,
                }));
            }
            cursor = seq_next(cursor);
            credit.store(cursor, Ordering::Release);
        }
        Ok(None)
    }

    /// Drains all publishers, including those with only traffic for other
    /// destinations. Call this on every wakeup (or from a polling loop) so
    /// idle peers do not hold back the shared ring. The handler receives a
    /// copy; it may retain or process the bytes before `output` is reused.
    pub fn poll(
        &mut self,
        output: &mut [u8; PAYLOAD_SIZE],
        mut handle: impl FnMut(Message, &[u8]),
    ) -> Result<usize, Error> {
        let mut count = 0;
        for publisher in 0..self.sections.len() {
            if publisher == self.id {
                continue;
            }
            while let Some(message) = self.try_receive(publisher as u16, output)? {
                handle(message, &output[..message.len]);
                count += 1;
            }
        }
        Ok(count)
    }

    fn validate_destination(&self, destination: u16) -> Result<(), Error> {
        self.ensure_ready()?;
        if destination != BROADCAST {
            let dst = destination as usize;
            if dst >= self.sections.len() || dst == self.id || !self.is_ready(dst) {
                return Err(Error::InvalidDestination);
            }
        }
        Ok(())
    }

    fn peer_count(&self) -> usize {
        self.sections.len()
    }

    fn id(&self) -> usize {
        self.id
    }

    fn is_peer_ready(&self, peer: usize) -> bool {
        self.is_ready(peer)
    }

    fn is_ready(&self, peer: usize) -> bool {
        self.states[peer].load(Ordering::Acquire) == READY
    }

    fn ensure_ready(&self) -> Result<(), Error> {
        if self.initialized && self.is_ready(self.id) {
            Ok(())
        } else {
            Err(Error::NotReady)
        }
    }

    fn check_section(&self, peer: usize) -> Result<(), Error> {
        if self.sections[peer].matches() {
            Ok(())
        } else {
            Err(Error::IncompatibleSection { peer: peer as u16 })
        }
    }
}

fn seq_next(seq: u32) -> u32 {
    if seq == MODULUS - 1 { 0 } else { seq + 1 }
}

fn seq_distance(from: u32, to: u32) -> u32 {
    if to >= from {
        to - from
    } else {
        MODULUS - from + to
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{align_of, offset_of, size_of};

    use super::*;

    #[test]
    fn peer_addressing_credit_backpressure_and_layout() {
        assert_eq!(size_of::<Section>(), SECTION_SIZE);
        assert_eq!(align_of::<Section>(), 256);
        assert_eq!(size_of::<SectionHeader>(), 256);
        assert_eq!(size_of::<Slot>(), 256);
        assert_eq!(offset_of!(Section, slots), 256);
        assert_eq!(offset_of!(SectionHeader, credit), 20);
        assert_eq!(offset_of!(Slot, payload), 16);

        let sections = [Section::new(), Section::new(), Section::new()];
        let states = [const { AtomicU32::new(0) }; 3];
        // SAFETY: Each endpoint owns a distinct section; backing remains live
        // throughout the test and the states govern publication.
        let mut peers = [
            unsafe { Peer::attach(0, &sections, &states).unwrap() },
            unsafe { Peer::attach(1, &sections, &states).unwrap() },
            unsafe { Peer::attach(2, &sections, &states).unwrap() },
        ];
        for (id, peer) in peers.iter_mut().enumerate() {
            peer.initialize().unwrap();
            states[id].store(READY, Ordering::Release);
        }
        let mut output = [0; PAYLOAD_SIZE];
        assert_eq!(
            peers[0].try_send(0, b"self"),
            Err(Error::InvalidDestination)
        );
        assert_eq!(
            peers[0].try_send(1, &[0; PAYLOAD_SIZE + 1]),
            Err(Error::PayloadTooLarge)
        );
        for n in 0..CAPACITY {
            peers[0].try_send(1, &[n as u8]).unwrap();
        }
        assert_eq!(
            peers[0].try_send(1, b"blocked"),
            Err(Error::Full { lagging_peer: 1 })
        );
        for n in 0..CAPACITY {
            let msg = peers[1].try_receive(0, &mut output).unwrap().unwrap();
            assert_eq!(
                (msg.source, msg.destination, msg.len, output[0]),
                (0, 1, 1, n as u8)
            );
        }
        // Peer 2 must skip traffic destined for peer 1 to release credit.
        assert_eq!(peers[2].poll(&mut output, |_, _| {}).unwrap(), 0);
        peers[0].try_send(2, b"two").unwrap();
        let msg = peers[2].try_receive(0, &mut output).unwrap().unwrap();
        assert_eq!(&output[..msg.len], b"two");
        peers[0].try_send(BROADCAST, b"all").unwrap();
        for peer in &mut peers[1..] {
            let msg = peer.try_receive(0, &mut output).unwrap().unwrap();
            assert_eq!(&output[..msg.len], b"all");
        }
    }

    #[test]
    fn joining_peer_starts_at_current_tail_and_wraps_without_aliasing_slots() {
        let sections = [Section::new(), Section::new()];
        let states = [const { AtomicU32::new(0) }; 2];
        // SAFETY: Distinct section owners and stable zero-initialized backing.
        let mut producer = unsafe { Peer::attach(0, &sections, &states).unwrap() };
        // SAFETY: Distinct section owners and stable zero-initialized backing.
        let mut consumer = unsafe { Peer::attach(1, &sections, &states).unwrap() };
        producer.initialize().unwrap();
        states[0].store(READY, Ordering::Release);
        // Simulate a long-lived producer just before the reserved sequence.
        sections[0]
            .header
            .tail
            .store(MODULUS - 1, Ordering::Relaxed);
        consumer.initialize().unwrap();
        states[1].store(READY, Ordering::Release);
        let mut output = [0; PAYLOAD_SIZE];
        producer.try_send(1, b"last").unwrap();
        producer.try_send(1, b"first").unwrap();
        for expected in [b"last".as_slice(), b"first".as_slice()] {
            let msg = consumer.try_receive(0, &mut output).unwrap().unwrap();
            assert_eq!(&output[..msg.len], expected);
        }
        assert_eq!(sections[0].header.tail.load(Ordering::Relaxed), 1);
    }
}
