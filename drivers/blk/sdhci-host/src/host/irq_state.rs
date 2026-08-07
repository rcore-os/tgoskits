use super::*;

const IRQ_GENERATION_SHIFT: u64 = 32;
const IRQ_NORMAL_MASK: u64 = 0xffff;
const IRQ_ERROR_SHIFT: u64 = 16;

pub(crate) struct IrqState {
    mailbox: AtomicU64,
    next_generation: AtomicU32,
}

impl IrqState {
    const fn new() -> Self {
        Self {
            mailbox: AtomicU64::new(0),
            next_generation: AtomicU32::new(0),
        }
    }

    pub(crate) fn begin_request(&self) {
        let generation = self.next_generation();
        self.mailbox
            .store(pack_mailbox(generation, 0, 0), Ordering::Release);
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

    pub(crate) fn take_normal(&self, mask: u16) -> u16 {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            let normal = mailbox_normal(cur);
            let taken = normal & mask;
            if taken == 0 {
                return 0;
            }
            let next = pack_mailbox(mailbox_generation(cur), normal & !mask, mailbox_error(cur));
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return taken,
                Err(observed) => cur = observed,
            }
        }
    }

    pub(crate) fn take_error_all(&self) -> u16 {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            let error = mailbox_error(cur);
            if error == 0 {
                return 0;
            }
            let next = pack_mailbox(mailbox_generation(cur), mailbox_normal(cur), 0);
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return error,
                Err(observed) => cur = observed,
            }
        }
    }

    pub(crate) fn clear_normal(&self, mask: u16) {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            let next = pack_mailbox(
                mailbox_generation(cur),
                mailbox_normal(cur) & !mask,
                mailbox_error(cur),
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

    pub(crate) fn clear_all(&self) {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            let next = pack_mailbox(mailbox_generation(cur), 0, 0);
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(observed) => cur = observed,
            }
        }
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
    pub(crate) state: IrqState,
}

impl IrqCore {
    pub(super) fn new(base_addr: usize) -> Self {
        Self {
            base_addr,
            state: IrqState::new(),
        }
    }
}
