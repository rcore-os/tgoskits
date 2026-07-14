//! Copy-only scheduler messages crossing CPU ownership boundaries.

use crate::{CpuId, ThreadId};

/// Class of owner-CPU work carried by one intrusive inbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum InboxKind {
    /// Make a sleeping or remotely queued thread runnable.
    RemoteWake,
    /// Transfer ownership after affinity or balancing selection.
    Migration,
    /// Reap a thread, coroutine, context, or other deferred resource.
    Reclaim,
}

/// Allocation-free scheduler request copied into owner CPU storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InboxMessage {
    kind: InboxKind,
    thread_id: ThreadId,
    source_cpu: u32,
    target_cpu: u32,
    generation: u64,
    payload: usize,
}

impl InboxMessage {
    const NO_CPU: u32 = u32::MAX;
    const BALANCE_REQUEST_FLAG: u64 = 1 << 63;

    /// Empty value used to initialize fixed drain buffers.
    pub const EMPTY: Self = Self {
        kind: InboxKind::Reclaim,
        thread_id: ThreadId::from_parts(0, 0),
        source_cpu: Self::NO_CPU,
        target_cpu: Self::NO_CPU,
        generation: 0,
        payload: 0,
    };

    /// Creates a direct remote wake request.
    pub const fn remote_wake(thread_id: ThreadId, target_cpu: CpuId) -> Self {
        Self::remote_wake_with_payload(thread_id, target_cpu, 0)
    }

    /// Creates a direct remote wake carrying a retained wake-header pointer.
    pub const fn remote_wake_with_payload(
        thread_id: ThreadId,
        target_cpu: CpuId,
        payload: usize,
    ) -> Self {
        Self {
            kind: InboxKind::RemoteWake,
            thread_id,
            source_cpu: Self::NO_CPU,
            target_cpu: target_cpu.as_u32(),
            generation: 0,
            payload,
        }
    }

    /// Creates an owner-to-owner migration transfer.
    pub const fn migration(
        thread_id: ThreadId,
        source_cpu: CpuId,
        target_cpu: CpuId,
        generation: u64,
    ) -> Self {
        Self::migration_with_payload(thread_id, source_cpu, target_cpu, generation, 0)
    }

    /// Creates a migration/policy-update transfer with retained payload data.
    pub const fn migration_with_payload(
        thread_id: ThreadId,
        source_cpu: CpuId,
        target_cpu: CpuId,
        generation: u64,
        payload: usize,
    ) -> Self {
        Self {
            kind: InboxKind::Migration,
            thread_id,
            source_cpu: source_cpu.as_u32(),
            target_cpu: target_cpu.as_u32(),
            generation,
            payload,
        }
    }

    /// Creates an idle-pull request sent to a remote runqueue owner.
    pub const fn balance_request(source_cpu: CpuId, target_cpu: CpuId, source_epoch: u64) -> Self {
        Self {
            kind: InboxKind::Migration,
            thread_id: ThreadId::from_parts(0, 0),
            source_cpu: source_cpu.as_u32(),
            target_cpu: target_cpu.as_u32(),
            generation: Self::BALANCE_REQUEST_FLAG | (source_epoch & !Self::BALANCE_REQUEST_FLAG),
            payload: 0,
        }
    }

    /// Reports whether this migration message asks the source owner to pull work.
    pub const fn is_balance_request(self) -> bool {
        self.kind as u8 == InboxKind::Migration as u8
            && self.generation & Self::BALANCE_REQUEST_FLAG != 0
            && self.payload == 0
    }

    /// Returns the source load-summary epoch observed by an idle requester.
    pub const fn balance_source_epoch(self) -> Option<u64> {
        if self.is_balance_request() {
            Some(self.generation & !Self::BALANCE_REQUEST_FLAG)
        } else {
            None
        }
    }

    /// Creates a deferred resource-reclaim request.
    pub const fn reclaim(thread_id: ThreadId, generation: u64, payload: usize) -> Self {
        Self {
            kind: InboxKind::Reclaim,
            thread_id,
            source_cpu: Self::NO_CPU,
            target_cpu: Self::NO_CPU,
            generation,
            payload,
        }
    }

    /// Returns the inbox class required by this request.
    pub const fn kind(self) -> InboxKind {
        self.kind
    }

    /// Returns the generation-checked destination thread.
    pub const fn thread_id(self) -> ThreadId {
        self.thread_id
    }

    /// Returns the source CPU for migration requests.
    pub const fn source_cpu(self) -> Option<CpuId> {
        if self.source_cpu == Self::NO_CPU {
            None
        } else {
            Some(CpuId::new(self.source_cpu))
        }
    }

    /// Returns the target CPU for wake and migration requests.
    pub const fn target_cpu(self) -> Option<CpuId> {
        if self.target_cpu == Self::NO_CPU {
            None
        } else {
            Some(CpuId::new(self.target_cpu))
        }
    }

    /// Returns the transfer or reclaim generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns opaque resource data for reclaim requests.
    pub const fn payload(self) -> usize {
        self.payload
    }
}
