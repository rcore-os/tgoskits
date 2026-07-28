//! Fixed-capacity generation-checked PMU sampling registry.

/// Maximum number of architecture PMU slots tracked by the registry.
pub(crate) const SAMPLE_SLOT_CAPACITY: usize = 32;

struct RegistryEntry<T> {
    generation: u64,
    value: T,
}

/// Failure to publish one PMU sampling slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegisterError {
    /// Counter index is outside the fixed registry.
    InvalidCounter,
    /// A live generation already owns this counter.
    Occupied,
}

/// Failure to remove one PMU sampling slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnregisterError {
    /// Counter index is outside the fixed registry.
    InvalidCounter,
    /// The slot is empty or belongs to a newer generation.
    Stale,
}

/// One CPU's PMU counter-to-sampling-output map.
pub(crate) struct SamplingRegistry<T> {
    slots: [Option<RegistryEntry<T>>; SAMPLE_SLOT_CAPACITY],
}

impl<T> SamplingRegistry<T> {
    /// Creates an empty fixed-capacity registry.
    pub(crate) const fn new() -> Self {
        Self {
            slots: [const { None }; SAMPLE_SLOT_CAPACITY],
        }
    }

    /// Publishes one generation without replacing an existing owner.
    pub(crate) fn register(
        &mut self,
        counter: usize,
        generation: u64,
        value: T,
    ) -> Result<(), RegisterError> {
        let slot = self
            .slots
            .get_mut(counter)
            .ok_or(RegisterError::InvalidCounter)?;
        if slot.is_some() {
            return Err(RegisterError::Occupied);
        }
        *slot = Some(RegistryEntry { generation, value });
        Ok(())
    }

    /// Returns the live value for IRQ service.
    pub(crate) fn get_mut(&mut self, counter: usize) -> Option<&mut T> {
        self.slots
            .get_mut(counter)
            .and_then(Option::as_mut)
            .map(|entry| &mut entry.value)
    }

    /// Removes exactly one generation and returns its owned value.
    pub(crate) fn unregister(
        &mut self,
        counter: usize,
        generation: u64,
    ) -> Result<T, UnregisterError> {
        let slot = self
            .slots
            .get_mut(counter)
            .ok_or(UnregisterError::InvalidCounter)?;
        if slot
            .as_ref()
            .is_none_or(|entry| entry.generation != generation)
        {
            return Err(UnregisterError::Stale);
        }
        Ok(slot.take().expect("validated PMU registry slot").value)
    }
}
