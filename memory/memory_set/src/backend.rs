use ax_memory_addr::MemoryAddr;

use crate::MappingResult;

/// Page-table state expected before a map operation commits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapPrecondition {
    /// The target range must not contain existing mappings.
    Vacant,
    /// Existing mappings are removed by earlier operations in this transaction.
    Replacing,
}

/// A page-table operation prepared by [`MappingBackend`].
#[derive(Clone, Copy, Debug)]
pub enum MappingOperation<A, F> {
    /// Add a mapping.
    Map {
        /// Start address.
        start: A,
        /// Mapping size.
        size: usize,
        /// Mapping flags.
        flags: F,
        /// Expected state of the target range during transaction preparation.
        precondition: MapPrecondition,
    },
    /// Remove a mapping.
    Unmap {
        /// Start address.
        start: A,
        /// Mapping size.
        size: usize,
        /// Flags recorded by the area being removed.
        old_flags: F,
    },
    /// Change mapping flags.
    Protect {
        /// Start address.
        start: A,
        /// Mapping size.
        size: usize,
        /// Flags to restore on rollback.
        old_flags: F,
        /// New mapping flags.
        new_flags: F,
    },
}

impl<A, F> MappingOperation<A, F> {
    /// Returns the start address and byte length affected by this operation.
    pub fn range(self) -> (A, usize) {
        match self {
            Self::Map { start, size, .. }
            | Self::Unmap { start, size, .. }
            | Self::Protect { start, size, .. } => (start, size),
        }
    }
}

/// Underlying operations to do when manipulating mappings within the specific
/// [`MemoryArea`](crate::MemoryArea).
///
/// The backend can be different for different memory areas. e.g., for linear
/// mappings, the target physical address is known when it is added to the page
/// table. For lazy mappings, an empty mapping needs to be added to the page
/// table to trigger a page fault.
pub trait MappingBackend: Clone {
    /// The address type used in the memory area.
    type Addr: MemoryAddr;
    /// The flags type used in the memory area.
    type Flags: Copy;
    /// The page table type used in the memory area.
    type PageTable;
    /// Resources and validation state reserved before a transaction commits.
    type MappingPlan;
    /// State required to roll back or finalize a committed operation.
    type CommitState;

    /// Validates an operation and reserves all resources it can require.
    ///
    /// This method must not change mappings or externally visible backend
    /// state. The caller must pass every uncommitted plan to [`Self::abort`].
    fn prepare(
        &self,
        operation: MappingOperation<Self::Addr, Self::Flags>,
        page_table: &mut Self::PageTable,
    ) -> MappingResult<Self::MappingPlan>;

    /// Releases a plan that will not be committed.
    fn abort(&self, plan: Self::MappingPlan, page_table: &mut Self::PageTable);

    /// Commits one prepared operation.
    ///
    /// A backend whose commit can fail must restore all changes made by that
    /// operation before returning the error. Earlier operations in the same
    /// transaction are rolled back by [`MemorySet`](crate::MemorySet).
    fn commit(
        &self,
        plan: Self::MappingPlan,
        page_table: &mut Self::PageTable,
    ) -> MappingResult<Self::CommitState>;

    /// Rolls back a successfully committed operation.
    fn rollback(&self, state: Self::CommitState, page_table: &mut Self::PageTable)
    -> MappingResult;

    /// Releases deferred resources after the whole transaction commits.
    fn finalize(&self, state: Self::CommitState, page_table: &mut Self::PageTable);

    /// Splits the backend into two backends at the given alignment difference.
    fn split(&mut self, align_diff: usize) -> Option<Self>;
}
