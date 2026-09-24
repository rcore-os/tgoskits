use rdif_gpu::{Completion, CompletionStatus, GpuDevice};

use crate::{DisplayError, DisplayEvent, DisplayState, DriverGeneric, OutputId, OutputInfo};

/// Display output and scanout control, independent of a GPU renderer.
pub trait DisplayController: DriverGeneric {
    fn output_count(&self) -> u32;
    fn output(&self, id: OutputId) -> Result<OutputInfo, DisplayError>;
    fn current_state(&self, id: OutputId) -> Result<Option<DisplayState>, DisplayError>;

    /// Validate the complete state without changing the current scanout,
    /// allocating a hardware resource or submitting a command. This is the
    /// `TEST_ONLY` path.
    fn check(&self, state: &DisplayState) -> Result<(), DisplayError>;

    /// Revalidate `state` under this exclusive access before touching hardware.
    /// On error the previous state and its backing remain active. On success
    /// the driver retains both new and old resources until the returned
    /// completion confirms that the old scanout is no longer used.
    fn commit(&mut self, state: &DisplayState) -> Result<Completion, DisplayError>;

    fn commit_status(&mut self, completion: Completion) -> Result<CompletionStatus, DisplayError>;

    /// Drain one event after the GPU control owner has serviced IRQ work.
    fn poll_event(&mut self) -> Option<DisplayEvent>;
}

/// One registered device implements both GPU and display capabilities.
/// A GPU without an output may be registered as `dyn GpuDevice` instead.
pub trait GpuDisplay: GpuDevice + DisplayController {}

impl<T: GpuDevice + DisplayController + ?Sized> GpuDisplay for T {}
