//! Pure control-plane state for one inherited perf-event family.

/// State inherited by a child at the relationship linearization point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FamilyJoinState {
    /// Root-fd enable intent.
    pub(crate) enabled: bool,
    /// Most recent root output publication, or `None` before mmap.
    pub(crate) output_generation: Option<u64>,
}

/// Bounded family-wide control state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfInheritanceLifecycle {
    enabled: bool,
    closed: bool,
    member_count: usize,
    output_generation: Option<u64>,
}

impl PerfInheritanceLifecycle {
    /// Creates one open family containing its root event.
    pub(crate) const fn new(enabled: bool) -> Self {
        Self {
            enabled,
            closed: false,
            member_count: 1,
            output_generation: None,
        }
    }

    /// Registers one child or rejects a child racing permanent close.
    pub(crate) fn register_member(&mut self, capacity: usize) -> Option<FamilyJoinState> {
        if self.closed || self.member_count >= capacity {
            return None;
        }
        self.member_count += 1;
        Some(FamilyJoinState {
            enabled: self.enabled,
            output_generation: self.output_generation,
        })
    }

    /// Retires one descendant after its owner-CPU PMU lease is quiescent.
    ///
    /// The root remains in the family until fd close. Returning `false` for a
    /// root-only family makes double retirement observable to the relationship
    /// owner instead of silently corrupting the capacity accounting.
    pub(crate) fn retire_member(&mut self) -> bool {
        if self.member_count <= 1 {
            return false;
        }
        self.member_count -= 1;
        true
    }

    /// Publishes a new root output generation to every current member.
    pub(crate) fn publish_output(&mut self) -> Option<u64> {
        if self.closed {
            return None;
        }
        let generation = self
            .output_generation
            .unwrap_or(0)
            .checked_add(1)
            .expect("perf output generation exhausted");
        self.output_generation = Some(generation);
        Some(generation)
    }

    /// Changes root-fd control intent and returns the bounded snapshot size.
    pub(crate) fn set_enabled(&mut self, enabled: bool) -> Option<usize> {
        if self.closed {
            return None;
        }
        self.enabled = enabled;
        Some(self.member_count)
    }

    /// Prevents future inheritance before teardown snapshots members.
    pub(crate) fn close(&mut self) -> usize {
        self.closed = true;
        self.enabled = false;
        self.member_count
    }

    pub(crate) const fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) const fn enabled(&self) -> bool {
        self.enabled
    }
}
