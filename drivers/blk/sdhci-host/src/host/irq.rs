//! IRQ generation handoff and acknowledged status mailbox.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::regs::NORMAL_INT_ERROR;

const IRQ_GENERATION_SHIFT: u64 = 32;
const IRQ_NORMAL_MASK: u64 = 0xffff;
const IRQ_ERROR_SHIFT: u64 = 16;

/// One indivisible observation of the SDHCI normal and error status banks.
///
/// The IRQ endpoint acknowledges both hardware registers before publishing
/// this value. Task context may consume individual normal-status causes, but
/// an error always consumes the entire snapshot because it terminates the
/// active request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct IrqSnapshot {
    pub(crate) generation: u32,
    pub(crate) normal: u16,
    pub(crate) error: u16,
}

impl IrqSnapshot {
    pub(crate) const fn empty() -> Self {
        Self {
            generation: 0,
            normal: 0,
            error: 0,
        }
    }

    fn from_mailbox(mailbox: u64) -> Self {
        Self {
            generation: mailbox_generation(mailbox),
            normal: mailbox_normal(mailbox),
            error: mailbox_error(mailbox),
        }
    }

    pub(crate) fn has_error(self) -> bool {
        self.normal & NORMAL_INT_ERROR != 0 || self.error != 0
    }

    pub(crate) fn is_empty(self) -> bool {
        self.normal == 0 && self.error == 0
    }

    pub(crate) fn merge(&mut self, incoming: Self) {
        if incoming.generation == 0 || (incoming.normal == 0 && incoming.error == 0) {
            return;
        }
        if self.generation != incoming.generation {
            *self = incoming;
            return;
        }
        self.normal |= incoming.normal;
        self.error |= incoming.error;
    }

    pub(crate) fn take(&mut self, normal_mask: u16) -> Self {
        if self.has_error() {
            let mut taken = *self;
            taken.normal |= NORMAL_INT_ERROR;
            self.normal = 0;
            self.error = 0;
            return taken;
        }

        let normal = self.normal & normal_mask;
        self.normal &= !normal_mask;
        Self {
            generation: self.generation,
            normal,
            error: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
enum IoStatusOwner {
    #[default]
    Initialization = 0,
    RuntimeIrq     = 1,
}

pub(crate) struct IrqState {
    mailbox: AtomicU64,
    next_generation: AtomicU32,
    status_owner: AtomicU8,
    delivery_enabled: AtomicBool,
}

impl IrqState {
    const fn new() -> Self {
        Self {
            mailbox: AtomicU64::new(0),
            next_generation: AtomicU32::new(0),
            status_owner: AtomicU8::new(IoStatusOwner::Initialization as u8),
            delivery_enabled: AtomicBool::new(false),
        }
    }

    pub(crate) fn begin_request(&self) -> bool {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        if mailbox_normal(cur) != 0 || mailbox_error(cur) != 0 {
            return false;
        }
        let generation = self.next_generation();
        loop {
            if mailbox_normal(cur) != 0 || mailbox_error(cur) != 0 {
                return false;
            }
            // Hand off only an empty mailbox. An old IRQ published before
            // this CAS makes the transition fail; one published afterwards
            // still carries the old generation and is rejected by
            // `cache_if_current`.
            let next = pack_mailbox(generation, 0, 0);
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(observed) => cur = observed,
            }
        }
    }

    pub(crate) fn end_request(&self) {
        self.mailbox.store(0, Ordering::Release);
    }

    pub(crate) fn cache_if_current(&self, generation: u32, normal: u16, error: u16) {
        if generation == 0 || (normal == 0 && error == 0) {
            return;
        }
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            if mailbox_generation(cur) != generation {
                return;
            }
            let next = pack_mailbox(
                generation,
                mailbox_normal(cur) | normal,
                mailbox_error(cur) | error,
            );
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(observed) => cur = observed,
            }
        }
    }

    pub(crate) fn generation(&self) -> u32 {
        mailbox_generation(self.mailbox.load(Ordering::Acquire))
    }

    pub(crate) fn request_handoff_ready(&self) -> bool {
        let mailbox = self.mailbox.load(Ordering::Acquire);
        mailbox_normal(mailbox) == 0 && mailbox_error(mailbox) == 0
    }

    pub(crate) fn take_snapshot(&self) -> IrqSnapshot {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            let snapshot = IrqSnapshot::from_mailbox(cur);
            if snapshot.normal == 0 && snapshot.error == 0 {
                return snapshot;
            }
            let next = pack_mailbox(snapshot.generation, 0, 0);
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return snapshot,
                Err(observed) => cur = observed,
            }
        }
    }

    fn set_status_owner(&self, owner: IoStatusOwner) {
        self.status_owner.store(owner as u8, Ordering::Release);
    }

    pub(super) fn enter_initialization_status_mode(&self) {
        self.set_status_owner(IoStatusOwner::Initialization);
    }

    pub(super) fn enter_runtime_irq_status_mode(&self) {
        self.set_status_owner(IoStatusOwner::RuntimeIrq);
    }

    fn status_owner(&self) -> IoStatusOwner {
        match self.status_owner.load(Ordering::Acquire) {
            value if value == IoStatusOwner::Initialization as u8 => IoStatusOwner::Initialization,
            value if value == IoStatusOwner::RuntimeIrq as u8 => IoStatusOwner::RuntimeIrq,
            _ => unreachable!("invalid SDHCI I/O status owner"),
        }
    }

    pub(super) fn runtime_irq_owned(&self) -> bool {
        self.status_owner() == IoStatusOwner::RuntimeIrq
    }

    pub(super) fn initialization_owned(&self) -> bool {
        self.status_owner() == IoStatusOwner::Initialization
    }

    pub(crate) fn set_delivery_enabled(&self, enabled: bool) {
        self.delivery_enabled.store(enabled, Ordering::Release);
    }

    pub(crate) fn delivery_enabled(&self) -> bool {
        self.delivery_enabled.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn pending_normal(&self) -> u16 {
        mailbox_normal(self.mailbox.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(crate) fn pending_error(&self) -> u16 {
        mailbox_error(self.mailbox.load(Ordering::Acquire))
    }

    fn next_generation(&self) -> u32 {
        let mut cur = self.next_generation.load(Ordering::Acquire);
        loop {
            let mut next = cur.wrapping_add(1);
            if next == 0 {
                next = 1;
            }
            match self.next_generation.compare_exchange_weak(
                cur,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return next,
                Err(observed) => cur = observed,
            }
        }
    }
}

fn pack_mailbox(generation: u32, normal: u16, error: u16) -> u64 {
    ((generation as u64) << IRQ_GENERATION_SHIFT)
        | normal as u64
        | ((error as u64) << IRQ_ERROR_SHIFT)
}

fn mailbox_generation(value: u64) -> u32 {
    (value >> IRQ_GENERATION_SHIFT) as u32
}

fn mailbox_normal(value: u64) -> u16 {
    (value & IRQ_NORMAL_MASK) as u16
}

fn mailbox_error(value: u64) -> u16 {
    ((value >> IRQ_ERROR_SHIFT) & IRQ_NORMAL_MASK) as u16
}

pub(crate) struct IrqCore {
    pub(crate) base_addr: usize,
    pub(crate) aligned_32bit: bool,
    pub(crate) state: IrqState,
}

impl IrqCore {
    pub(super) fn new(base_addr: usize, aligned_32bit: bool) -> Self {
        Self {
            base_addr,
            aligned_32bit,
            state: IrqState::new(),
        }
    }
}
