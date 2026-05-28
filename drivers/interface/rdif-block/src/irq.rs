use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqSourceInfo {
    pub id: usize,
    pub queues: IdList,
}

impl IrqSourceInfo {
    pub const fn new(id: usize, queues: IdList) -> Self {
        Self { id, queues }
    }

    pub const fn legacy(queues: IdList) -> Self {
        Self { id: 0, queues }
    }
}

pub type IrqSourceList = Vec<IrqSourceInfo>;

pub trait IrqHandler: Send + Sync + 'static {
    fn handle_irq(&self) -> Event;
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdList(u64);

impl IdList {
    pub const fn none() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub fn contains(&self, id: usize) -> bool {
        id < 64 && (self.0 & (1 << id)) != 0
    }

    pub fn insert(&mut self, id: usize) {
        if id < 64 {
            self.0 |= 1 << id;
        }
    }

    pub fn remove(&mut self, id: usize) {
        if id < 64 {
            self.0 &= !(1 << id);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> {
        (0..64).filter(move |i| self.contains(*i))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub queues: IdList,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            queues: IdList::none(),
        }
    }

    pub const fn from_queue_bits(bits: u64) -> Self {
        Self {
            queues: IdList::from_bits(bits),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_source_lists_queue_masks() {
        let mut queues = IdList::none();
        queues.insert(2);
        let source = IrqSourceInfo::legacy(queues);

        assert_eq!(source.id, 0);
        assert!(source.queues.contains(2));
    }
}
