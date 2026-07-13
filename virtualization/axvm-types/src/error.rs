// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Architecture backend error contract shared with AxVM.

/// Result returned by architecture virtualization backends.
pub type VmBackendResult<T = ()> = Result<T, VmBackendError>;

/// Failures reported by an architecture virtualization backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VmBackendError {
    /// The backend rejected a caller-provided value.
    #[error("invalid virtualization backend input")]
    InvalidInput,
    /// The backend received malformed or inconsistent architecture data.
    #[error("invalid virtualization backend data")]
    InvalidData,
    /// The backend state does not permit the requested operation.
    #[error("invalid virtualization backend state")]
    InvalidState,
    /// The backend does not implement the requested operation.
    #[error("unsupported virtualization backend operation")]
    Unsupported,
    /// The backend could not allocate the required memory.
    #[error("virtualization backend memory allocation failed")]
    OutOfMemory,
    /// A backend resource is already owned or in use.
    #[error("virtualization backend resource is busy")]
    ResourceBusy,
}
