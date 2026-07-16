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

const HOST_ARCEOS: &str = include_str!("../src/host/arceos.rs");
const HOST_TASK: &str = include_str!("../src/host/task.rs");
const TASK: &str = include_str!("../src/task.rs");
const ARCH: &str = include_str!("../src/arch/mod.rs");

#[test]
fn current_task_lookup_preserves_runtime_errors() {
    assert!(HOST_ARCEOS.contains(
        "pub(crate) fn try_current_task() -> Result<Option<ArceOsCurrentTask>, ArceOsTaskError>"
    ));
    assert!(!HOST_ARCEOS.contains("current_thread_handle()\n        .ok()"));
    assert!(
        HOST_TASK
            .contains("pub(crate) fn try_current_task() -> Result<Option<CurrentTask>, TaskError>")
    );
}

#[test]
fn vcpu_extension_lookup_validates_matching_extension_data() {
    assert!(TASK.contains("fn try_as_vcpu_task(&self) -> Result<Option<&VCpuTask>, TaskError>"));
    assert!(TASK.contains("TaskError::InvalidRuntimeHandle"));
    assert!(TASK.contains("NonNull::<VCpuTask>::new"));
    assert!(TASK.contains("is_aligned"));
    assert!(!TASK.contains("let extension = self.extension()?;\n        if"));
}

#[test]
fn deferred_arch_identity_keeps_absence_separate_from_corruption() {
    assert!(ARCH.contains(
        "current_vcpu_identity_for_task() -> Result<Option<VcpuExecutionIdentity>, TaskError>"
    ));
    assert!(ARCH.contains("let Some(current) = crate::host::task::try_current_task()? else"));
    assert!(ARCH.contains("let Some(task) = current.try_as_vcpu_task()? else"));
}
