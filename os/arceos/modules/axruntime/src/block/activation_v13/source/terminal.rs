//! Terminal close for a synchronized IRQ source owner.

use alloc::boxed::Box;
use core::fmt;

use super::{close_action_then_retire, suspended::QuiescedSourceReady};
use crate::maintenance::MaintenanceError;

impl QuiescedSourceReady {
    /// Closes the synchronized action for a terminal shutdown instead of
    /// rearming it for controller reinitialization.
    pub(in crate::block::activation_v13) fn close_after_quiesce(
        self,
    ) -> Result<(), QuiescedSourceCloseFailure> {
        let Self {
            source,
            ingress,
            control,
            action,
            platform_source,
            driver_retired,
        } = self;
        match close_action_then_retire(
            action,
            platform_source,
            |action| {
                action.close().map_err(|failure| {
                    let (error, action) = failure.into_parts();
                    (error, Box::new(action))
                })
            },
            |platform_source| {
                // SAFETY: the synchronized action was removed and its
                // callback destroyed before this exact vector is retired.
                unsafe { platform_source.retire_after_action_close() }
            },
        ) {
            Ok(()) => Ok(()),
            Err(failure) => Err(QuiescedSourceCloseFailure {
                error: failure.error,
                source: Box::new(Self {
                    source,
                    ingress,
                    control,
                    action: *failure.action,
                    platform_source: failure.platform_source,
                    driver_retired,
                }),
            }),
        }
    }
}

/// Failed terminal close retaining the synchronized source owner.
#[must_use = "retry close or quarantine the complete synchronized source owner"]
pub(in crate::block::activation_v13) struct QuiescedSourceCloseFailure {
    error: MaintenanceError,
    source: Box<QuiescedSourceReady>,
}

impl QuiescedSourceCloseFailure {
    pub(in crate::block::activation_v13) fn into_parts(
        self,
    ) -> (MaintenanceError, QuiescedSourceReady) {
        (self.error, *self.source)
    }
}

impl fmt::Debug for QuiescedSourceCloseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuiescedSourceCloseFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for QuiescedSourceCloseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block IRQ synchronized source close failed: {}",
            self.error
        )
    }
}

impl core::error::Error for QuiescedSourceCloseFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}
