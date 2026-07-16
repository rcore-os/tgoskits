//! Stable, backend-independent state published for the current host CPU.

use core::sync::atomic::{AtomicU64, Ordering};

use axvm_types::{VCpuId, VMId};

const INTERRUPTS_PER_WORD: usize = u64::BITS as usize;
const PENDING_INTERRUPT_WORDS: usize = 4;
const PENDING_INTERRUPT_CAPACITY: usize = INTERRUPTS_PER_WORD * PENDING_INTERRUPT_WORDS;

/// Copyable identity of the vCPU currently owned by one host CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CurrentVcpuIdentity {
    vm_id: VMId,
    vcpu_id: VCpuId,
    generation: u64,
}

impl CurrentVcpuIdentity {
    /// Returns the owning VM identifier.
    pub(crate) const fn vm_id(self) -> VMId {
        self.vm_id
    }

    /// Returns the vCPU identifier within the VM.
    pub(crate) const fn vcpu_id(self) -> VCpuId {
        self.vcpu_id
    }

    /// Returns the publication generation observed with this identity.
    pub(crate) const fn generation(self) -> u64 {
        self.generation
    }
}

/// Stable header visible to hard IRQ code while a vCPU owns the current CPU.
///
/// The header deliberately contains no architecture backend pointer. Interrupt
/// producers only coalesce a bounded vector into the preallocated bitmap; the
/// owner drains that bitmap while holding its exclusive backend token.
#[repr(C, align(64))]
pub(crate) struct CurrentVcpuHeader {
    vm_id: VMId,
    vcpu_id: VCpuId,
    generation: AtomicU64,
    pending_interrupts: [AtomicU64; PENDING_INTERRUPT_WORDS],
}

impl CurrentVcpuHeader {
    /// Creates the stable header embedded in one vCPU allocation.
    pub(crate) const fn new(vm_id: VMId, vcpu_id: VCpuId) -> Self {
        Self {
            vm_id,
            vcpu_id,
            generation: AtomicU64::new(0),
            pending_interrupts: [const { AtomicU64::new(0) }; PENDING_INTERRUPT_WORDS],
        }
    }

    /// Starts a new CPU-local publication and returns its nonzero generation.
    pub(crate) fn begin_publication(&self) -> u64 {
        self.generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    /// Copies the identity visible to an IRQ-side observer.
    pub(crate) fn identity(&self) -> CurrentVcpuIdentity {
        CurrentVcpuIdentity {
            vm_id: self.vm_id,
            vcpu_id: self.vcpu_id,
            generation: self.generation.load(Ordering::Acquire),
        }
    }

    /// Coalesces one interrupt vector without allocation, locking, or callback.
    pub(crate) fn publish_interrupt(&self, vector: usize) -> Result<(), CurrentVcpuInterruptError> {
        if vector >= PENDING_INTERRUPT_CAPACITY {
            return Err(CurrentVcpuInterruptError::VectorOutOfRange { vector });
        }
        let word = vector / INTERRUPTS_PER_WORD;
        let bit = vector % INTERRUPTS_PER_WORD;
        self.pending_interrupts[word].fetch_or(1_u64 << bit, Ordering::Release);
        Ok(())
    }

    /// Drains pending vectors through the owner-only backend access path.
    ///
    /// A failed vector and every not-yet-delivered vector from the same word
    /// are atomically republished, so a transient backend error cannot lose an
    /// interrupt racing with a hard IRQ producer.
    pub(crate) fn drain_pending<E>(
        &self,
        mut inject: impl FnMut(usize) -> Result<(), E>,
    ) -> Result<(), E> {
        for (word_index, word) in self.pending_interrupts.iter().enumerate() {
            let mut pending = word.swap(0, Ordering::AcqRel);
            while pending != 0 {
                let bit = pending.trailing_zeros() as usize;
                if let Err(error) = inject(word_index * INTERRUPTS_PER_WORD + bit) {
                    word.fetch_or(pending, Ordering::Release);
                    return Err(error);
                }
                pending &= !(1_u64 << bit);
            }
        }
        Ok(())
    }
}

/// Failure to publish a bounded hard-IRQ vCPU interrupt request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CurrentVcpuInterruptError {
    /// The architecture-neutral pending bitmap represents vectors 0 through 255.
    #[error("vCPU interrupt vector {vector:#x} exceeds the fixed pending bitmap")]
    VectorOutOfRange {
        /// Rejected interrupt vector.
        vector: usize,
    },
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn duplicate_irq_publication_is_coalesced_without_losing_other_vectors() {
        let header = CurrentVcpuHeader::new(3, 7);
        header.publish_interrupt(9).unwrap();
        header.publish_interrupt(9).unwrap();
        header.publish_interrupt(130).unwrap();
        let mut delivered = Vec::new();

        header
            .drain_pending::<core::convert::Infallible>(|vector| {
                delivered.push(vector);
                Ok(())
            })
            .unwrap();

        assert_eq!(delivered, [9, 130]);
    }

    #[test]
    fn publication_identity_is_copied_with_a_monotonic_generation() {
        let header = CurrentVcpuHeader::new(3, 7);

        let first = header.begin_publication();
        let first_identity = header.identity();
        let second = header.begin_publication();
        let second_identity = header.identity();

        assert_eq!((first_identity.vm_id(), first_identity.vcpu_id()), (3, 7));
        assert_eq!(first_identity.generation(), first);
        assert_eq!(second_identity.generation(), second);
        assert_eq!(second, first + 1);
    }

    #[test]
    fn failed_delivery_republishes_the_failed_and_remaining_vectors() {
        let header = CurrentVcpuHeader::new(3, 7);
        header.publish_interrupt(4).unwrap();
        header.publish_interrupt(5).unwrap();

        assert_eq!(header.drain_pending(|_| Err("busy")), Err("busy"));

        let mut delivered = Vec::new();
        header
            .drain_pending::<core::convert::Infallible>(|vector| {
                delivered.push(vector);
                Ok(())
            })
            .unwrap();
        assert_eq!(delivered, [4, 5]);
    }

    #[test]
    fn out_of_range_vector_is_rejected_without_mutating_the_bitmap() {
        let header = CurrentVcpuHeader::new(3, 7);

        assert_eq!(
            header.publish_interrupt(PENDING_INTERRUPT_CAPACITY),
            Err(CurrentVcpuInterruptError::VectorOutOfRange {
                vector: PENDING_INTERRUPT_CAPACITY,
            })
        );
        header
            .drain_pending::<core::convert::Infallible>(|_| panic!("bitmap must remain empty"))
            .unwrap();
    }
}
