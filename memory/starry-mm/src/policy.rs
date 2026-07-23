use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
extern crate std;

static GLOBAL_COMMIT: CommitAccounting = CommitAccounting::new(0);

#[cfg(test)]
pub(crate) static GLOBAL_COMMIT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Linux committed-memory treatment for one VMA backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitKind {
    /// File-backed, device, or otherwise externally owned memory.
    Unaccounted,
    /// A private anonymous mapping that requires backing only while writable.
    PrivateAnonymous,
    /// A private file mapping that requires COW backing only while writable.
    PrivateFile,
}

impl CommitKind {
    /// Returns the bytes charged to `Committed_AS` for this mapping.
    pub const fn accounted_bytes(self, writable: bool, bytes: u64) -> u64 {
        match self {
            Self::Unaccounted => 0,
            Self::PrivateAnonymous if writable => bytes,
            Self::PrivateAnonymous => 0,
            Self::PrivateFile if writable => bytes,
            Self::PrivateFile => 0,
        }
    }
}

/// Linux `vm.overcommit_memory` mode exposed by this build.
pub const fn overcommit_memory_mode() -> u8 {
    if cfg!(feature = "starry-strict-commit") {
        2
    } else {
        1
    }
}

/// Error returned by virtual-memory admission policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    /// The process virtual address-space limit would be exceeded.
    #[error("virtual address-space limit exceeded")]
    AddressLimit,
    /// Strict committed-memory admission would be exceeded.
    #[error("committed-memory limit exceeded")]
    CommitLimit,
    /// Arithmetic overflow made the request invalid.
    #[error("virtual-memory accounting overflow")]
    Overflow,
    /// A transaction attempted to release more commit than the address space owns.
    #[error("address-space commit accounting underflow")]
    AccountingUnderflow,
}

/// Checks a prospective VMA replacement against `RLIMIT_AS`.
pub fn admit_address_space(
    current_bytes: u64,
    replaced_bytes: u64,
    requested_bytes: u64,
    limit: u64,
) -> Result<u64, AdmissionError> {
    let retained = current_bytes
        .checked_sub(replaced_bytes)
        .ok_or(AdmissionError::AccountingUnderflow)?;
    let next = retained
        .checked_add(requested_bytes)
        .ok_or(AdmissionError::Overflow)?;
    if limit != u64::MAX && next > limit {
        return Err(AdmissionError::AddressLimit);
    }
    Ok(next)
}

/// System-wide committed-memory accounting.
///
/// Both policies maintain `Committed_AS`; only strict mode rejects a charge
/// that exceeds `limit`.
pub struct CommitAccounting {
    committed: AtomicU64,
    limit: AtomicU64,
}

impl CommitAccounting {
    /// Creates accounting with the supplied strict commit limit.
    pub const fn new(limit: u64) -> Self {
        Self {
            committed: AtomicU64::new(0),
            limit: AtomicU64::new(limit),
        }
    }

    /// Updates the strict admission limit.
    pub fn set_limit(&self, limit: u64) {
        self.limit.store(limit, Ordering::Release);
    }

    /// Returns currently admitted bytes.
    pub fn committed(&self) -> u64 {
        self.committed.load(Ordering::Acquire)
    }

    /// Returns the configured strict-admission limit in bytes.
    pub fn limit(&self) -> u64 {
        self.limit.load(Ordering::Acquire)
    }

    /// Releases bytes retained by a committed reservation.
    pub fn release(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.committed
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_sub(bytes)
            })
            .expect("committed-memory release must not exceed the retained charge");
    }

    /// Reserves commit and returns a move-only charge released on drop.
    pub fn admit(&self, bytes: u64) -> Result<CommitCharge<'_>, AdmissionError> {
        let limit = self.limit.load(Ordering::Acquire);
        let mut current = self.committed.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(bytes).ok_or(AdmissionError::Overflow)?;
            if cfg!(feature = "starry-strict-commit") && next > limit {
                return Err(AdmissionError::CommitLimit);
            }
            match self.committed.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        Ok(CommitCharge {
            accounting: self,
            bytes,
        })
    }
}

/// Ownership token for an admitted strict commit charge.
pub struct CommitCharge<'a> {
    accounting: &'a CommitAccounting,
    bytes: u64,
}

impl CommitCharge<'_> {
    /// Retains this charge after the transaction commits.
    pub fn retain(mut self) -> u64 {
        let bytes = self.bytes;
        self.bytes = 0;
        bytes
    }
}

impl Drop for CommitCharge<'_> {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.accounting
                .committed
                .fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

/// Configures strict committed-memory admission from the runtime allocator's
/// managed capacity.
pub fn configure_commit_limit(limit: u64) {
    GLOBAL_COMMIT.set_limit(limit);
}

/// Reserves bytes for a pending address-space transaction.
pub fn reserve_commit(bytes: u64) -> Result<CommitCharge<'static>, AdmissionError> {
    GLOBAL_COMMIT.admit(bytes)
}

/// Releases bytes owned by a committed address space.
pub fn release_commit(bytes: u64) {
    GLOBAL_COMMIT.release(bytes);
}

/// Returns the system-wide committed byte count.
pub fn committed_bytes() -> u64 {
    GLOBAL_COMMIT.committed()
}

/// Returns the configured system-wide commit limit in bytes.
pub fn commit_limit_bytes() -> u64 {
    GLOBAL_COMMIT.limit()
}

/// Commit bytes owned by one address space.
pub struct AddressSpaceCommit {
    bytes: u64,
}

impl AddressSpaceCommit {
    /// Creates an empty address-space commit ledger.
    pub const fn new() -> Self {
        Self { bytes: 0 }
    }

    /// Returns the bytes owned by this address space.
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Prepares a commit-accounting delta without changing the ledger.
    pub fn prepare_delta(&self, removed: u64, added: u64) -> Result<CommitDelta, AdmissionError> {
        let retained = self
            .bytes
            .checked_sub(removed)
            .ok_or(AdmissionError::AccountingUnderflow)?;
        let next = retained
            .checked_add(added)
            .ok_or(AdmissionError::Overflow)?;
        if next == self.bytes {
            return Ok(CommitDelta::Unchanged);
        }
        if next > self.bytes {
            return reserve_commit(next - self.bytes).map(CommitDelta::Increase);
        }
        Ok(CommitDelta::Decrease(self.bytes - next))
    }

    /// Reserves the commit required by a fork clone.
    pub fn reserve_clone(&self) -> Result<CommitCharge<'static>, AdmissionError> {
        reserve_commit(self.bytes)
    }

    /// Adopts a prepared fork-clone charge.
    pub fn adopt(&mut self, charge: CommitCharge<'static>) {
        debug_assert_eq!(self.bytes, 0, "adopting commit into a non-empty ledger");
        self.bytes = charge.retain();
    }

    /// Releases all commit owned by this address space.
    pub fn clear(&mut self) {
        release_commit(self.bytes);
        self.bytes = 0;
    }
}

impl Default for AddressSpaceCommit {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AddressSpaceCommit {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Prepared address-space commit update.
pub enum CommitDelta {
    Unchanged,
    Increase(CommitCharge<'static>),
    Decrease(u64),
}

impl CommitDelta {
    /// Applies the prepared delta after the VMA/PTE transaction commits.
    pub fn commit(self, ledger: &mut AddressSpaceCommit) {
        match self {
            Self::Unchanged => {}
            Self::Increase(charge) => ledger.bytes += charge.retain(),
            Self::Decrease(bytes) => {
                ledger.bytes -= bytes;
                release_commit(bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn address_limit_accounts_only_the_replacement_delta() {
        assert_eq!(admit_address_space(100, 40, 60, 120), Ok(120));
        assert_eq!(
            admit_address_space(100, 0, 21, 120),
            Err(AdmissionError::AddressLimit),
        );
        assert_eq!(admit_address_space(100, 0, 21, u64::MAX), Ok(121));
        assert_eq!(
            admit_address_space(100, 101, 0, u64::MAX),
            Err(AdmissionError::AccountingUnderflow),
        );
    }

    #[test]
    fn overcommit_mode_matches_the_compiled_policy() {
        assert_eq!(
            overcommit_memory_mode(),
            if cfg!(feature = "starry-strict-commit") {
                2
            } else {
                1
            },
        );
    }

    #[test]
    fn commit_kind_charges_only_memory_promised_by_the_kernel() {
        assert_eq!(CommitKind::Unaccounted.accounted_bytes(true, 4096), 0);
        assert_eq!(CommitKind::PrivateAnonymous.accounted_bytes(false, 4096), 0);
        assert_eq!(
            CommitKind::PrivateAnonymous.accounted_bytes(true, 4096),
            4096
        );
        assert_eq!(CommitKind::PrivateFile.accounted_bytes(false, 4096), 0);
        assert_eq!(CommitKind::PrivateFile.accounted_bytes(true, 4096), 4096);
    }

    #[cfg(feature = "starry-strict-commit")]
    #[test]
    fn strict_commit_rejects_an_increase_above_the_limit() {
        let accounting = CommitAccounting::new(4096);
        let retained = accounting.admit(4096).unwrap().retain();

        assert!(matches!(
            accounting.admit(1),
            Err(AdmissionError::CommitLimit)
        ));
        assert_eq!(accounting.committed(), retained);
        accounting.release(retained);
    }

    #[test]
    fn retained_commit_is_released_explicitly() {
        let accounting = CommitAccounting::new(16);
        let retained = accounting.admit(8).unwrap().retain();
        assert_eq!(retained, 8);
        assert_eq!(accounting.committed(), retained);
        accounting.release(retained);
        assert_eq!(accounting.committed(), 0);
    }

    #[test]
    fn rejected_release_does_not_wrap_committed_bytes() {
        let accounting = CommitAccounting::new(16);
        let retained = accounting.admit(8).unwrap().retain();

        let result = std::panic::catch_unwind(|| accounting.release(retained + 1));

        assert!(result.is_err());
        assert_eq!(accounting.committed(), retained);
        accounting.release(retained);
    }

    #[cfg(not(feature = "starry-strict-commit"))]
    #[test]
    fn always_overcommit_still_tracks_committed_bytes() {
        let accounting = CommitAccounting::new(1);
        let retained = accounting.admit(8).unwrap().retain();

        assert_eq!(retained, 8);
        assert_eq!(accounting.committed(), 8);
        accounting.release(retained);
        assert_eq!(accounting.committed(), 0);
    }

    #[test]
    fn address_space_commit_delta_is_transactional() {
        let _guard = GLOBAL_COMMIT_TEST_LOCK.lock().unwrap();
        configure_commit_limit(16);
        let mut ledger = AddressSpaceCommit::new();
        let increase = ledger.prepare_delta(0, 8).unwrap();
        assert_eq!(ledger.bytes(), 0);
        increase.commit(&mut ledger);
        let expected = 8;
        assert_eq!(ledger.bytes(), expected);

        if expected != 0 {
            let decrease = ledger.prepare_delta(8, 3).unwrap();
            assert_eq!(ledger.bytes(), 8);
            decrease.commit(&mut ledger);
            assert_eq!(ledger.bytes(), 3);
        }
        ledger.clear();
        assert_eq!(ledger.bytes(), 0);
    }

    #[test]
    fn commit_delta_rejects_removing_unowned_bytes() {
        let ledger = AddressSpaceCommit::new();

        assert!(matches!(
            ledger.prepare_delta(1, 1),
            Err(AdmissionError::AccountingUnderflow)
        ));
        assert!(matches!(
            ledger.prepare_delta(1, 2),
            Err(AdmissionError::AccountingUnderflow)
        ));
    }
}
