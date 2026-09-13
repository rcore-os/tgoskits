//! Single-owner SDIO and AIC state-machine progression.

mod operation;
mod output;
mod progress;

pub(crate) use operation::{ActiveOperation, OperationCompletion};
pub(crate) use progress::{AicOwner, OwnerProgress, OwnerWait};
