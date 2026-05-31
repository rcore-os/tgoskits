// Copyright 2025 The Axvisor Team
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

//! Host process lifecycle APIs for AxVisor.

/// Process-lifecycle APIs required by AxVisor.
#[crate::api_def]
pub trait ProcessIf {
    /// Terminates the current host process/runtime with `exit_code`.
    fn exit(exit_code: i32) -> !;
}
