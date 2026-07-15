use alloc::vec::Vec;

use crate::{AxVmError, AxVmResult, GuestPhysAddr};

enum BootMemoryWrite {
    Bytes {
        gpa: GuestPhysAddr,
        bytes: Vec<u8>,
    },
    Fill {
        gpa: GuestPhysAddr,
        size: usize,
        byte: u8,
    },
}

enum SnapshotState {
    Recording(Vec<BootMemoryWrite>),
    Complete(Vec<BootMemoryWrite>),
}

pub(crate) struct BootMemorySnapshot {
    state: SnapshotState,
}

impl BootMemorySnapshot {
    pub(crate) const fn new() -> Self {
        Self {
            state: SnapshotState::Recording(Vec::new()),
        }
    }

    pub(crate) fn record_bytes(&mut self, gpa: GuestPhysAddr, bytes: &[u8]) -> AxVmResult {
        self.record(BootMemoryWrite::Bytes {
            gpa,
            bytes: bytes.to_vec(),
        })
    }

    pub(crate) fn record_fill(&mut self, gpa: GuestPhysAddr, size: usize, byte: u8) -> AxVmResult {
        self.record(BootMemoryWrite::Fill { gpa, size, byte })
    }

    fn record(&mut self, write: BootMemoryWrite) -> AxVmResult {
        match &mut self.state {
            SnapshotState::Recording(writes) => {
                writes.push(write);
                Ok(())
            }
            SnapshotState::Complete(_) => Err(AxVmError::invalid_state(
                "record boot memory",
                "boot memory snapshot is already complete",
            )),
        }
    }

    pub(crate) fn finish(&mut self) -> AxVmResult {
        match &mut self.state {
            SnapshotState::Recording(writes) => {
                self.state = SnapshotState::Complete(core::mem::take(writes));
                Ok(())
            }
            SnapshotState::Complete(_) => Err(AxVmError::invalid_state(
                "finish boot memory snapshot",
                "boot memory snapshot is already complete",
            )),
        }
    }

    pub(crate) fn restore_with(
        &self,
        clear: impl FnOnce() -> AxVmResult,
        mut write_bytes: impl FnMut(GuestPhysAddr, &[u8]) -> AxVmResult,
        mut fill: impl FnMut(GuestPhysAddr, usize, u8) -> AxVmResult,
    ) -> AxVmResult {
        let SnapshotState::Complete(writes) = &self.state else {
            return Err(AxVmError::invalid_state(
                "restore boot memory",
                "boot memory snapshot is not complete",
            ));
        };

        clear()?;
        for write in writes {
            match write {
                BootMemoryWrite::Bytes { gpa, bytes } => write_bytes(*gpa, bytes)?,
                BootMemoryWrite::Fill { gpa, size, byte } => fill(*gpa, *size, *byte)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::cell::RefCell;

    use super::BootMemorySnapshot;
    use crate::{AxVmError, GuestPhysAddr};

    #[test]
    fn boot_memory_snapshot_requires_completion_before_restore() {
        let snapshot = BootMemorySnapshot::new();
        let result = snapshot.restore_with(|| Ok(()), |_, _| Ok(()), |_, _, _| Ok(()));
        assert!(matches!(result, Err(AxVmError::InvalidState { .. })));
    }

    #[test]
    fn boot_memory_snapshot_clears_then_replays_ordered_writes() {
        let mut snapshot = BootMemorySnapshot::new();
        snapshot
            .record_bytes(GuestPhysAddr::from(2), &[1, 2, 3])
            .unwrap();
        snapshot.record_fill(GuestPhysAddr::from(3), 2, 9).unwrap();
        snapshot.finish().unwrap();

        let memory = RefCell::new(alloc::vec![0xaa; 8]);
        let events = RefCell::new(alloc::vec::Vec::new());
        snapshot
            .restore_with(
                || {
                    events.borrow_mut().push("clear");
                    memory.borrow_mut().fill(0);
                    Ok(())
                },
                |gpa, bytes| {
                    events.borrow_mut().push("bytes");
                    let start = gpa.as_usize();
                    memory.borrow_mut()[start..start + bytes.len()].copy_from_slice(bytes);
                    Ok(())
                },
                |gpa, size, byte| {
                    events.borrow_mut().push("fill");
                    let start = gpa.as_usize();
                    memory.borrow_mut()[start..start + size].fill(byte);
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(&*events.borrow(), &["clear", "bytes", "fill"]);
        assert_eq!(&*memory.borrow(), &[0, 0, 1, 9, 9, 0, 0, 0]);
    }

    #[test]
    fn boot_memory_snapshot_rejects_writes_after_completion() {
        let mut snapshot = BootMemorySnapshot::new();
        snapshot.finish().unwrap();
        let result = snapshot.record_bytes(GuestPhysAddr::from(0), &[1]);
        assert!(matches!(result, Err(AxVmError::InvalidState { .. })));
    }
}
