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

#[test]
fn axvm_owns_its_public_error_contract() {
    let manifest = include_str!("../Cargo.toml");
    let crate_root = include_str!("../src/lib.rs");
    let error = include_str!("../src/error.rs");

    assert!(!manifest.contains("ax-errno"));
    assert!(manifest.contains("thiserror = { workspace = true }"));
    assert!(crate_root.contains("pub use error::{AxVmError, AxVmResult};"));
    assert!(error.contains("thiserror::Error"));
    assert!(error.contains("pub enum AxVmError"));
    assert!(error.contains("pub type AxVmResult<T = ()>"));
}

#[test]
fn axvm_sources_do_not_name_axerrno() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![source_root];
    let mut violations = Vec::new();

    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).expect("AxVM source directory must be readable") {
            let entry = entry.expect("AxVM source entry must be readable");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().is_some_and(|extension| extension == "rs") {
                let source = std::fs::read_to_string(&path).expect("AxVM source must be readable");
                if source.contains("ax_errno") {
                    violations.push(path);
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "AxVM sources must not name ax_errno directly: {violations:?}"
    );
}

#[test]
fn public_failure_interfaces_name_axvm_result() {
    let runtime = include_str!("../src/runtime/mod.rs");
    let vm = include_str!("../src/vm/mod.rs");
    let prepare = include_str!("../src/vm/prepare.rs");
    let boot = include_str!("../src/boot/mod.rs");

    assert!(runtime.contains("pub fn start_vm(vm_id: usize) -> AxVmResult"));
    assert!(vm.contains("pub fn new(config: AxVMConfig) -> AxVmResult<AxVMRef>"));
    assert!(prepare.contains("pub fn prepare(&self) -> AxVmResult"));
    assert!(boot.contains("fn read_file(&self, file_name: &str) -> crate::AxVmResult"));
}

#[test]
fn axvisor_uses_anyhow_without_axerrno() {
    let axvisor_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../os/axvisor");
    let manifest = std::fs::read_to_string(axvisor_root.join("Cargo.toml"))
        .expect("AxVisor manifest must be readable");
    let manager = std::fs::read_to_string(axvisor_root.join("src/manager.rs"))
        .expect("AxVisor manager source must be readable");
    let config = std::fs::read_to_string(axvisor_root.join("src/config.rs"))
        .expect("AxVisor config source must be readable");
    let shell = std::fs::read_to_string(axvisor_root.join("src/shell/command/vm.rs"))
        .expect("AxVisor shell source must be readable");

    assert!(!manifest.contains("ax-errno"));
    assert!(manifest.contains("anyhow.workspace = true"));
    assert!(!manager.contains("ax_errno"));
    assert!(!config.contains("ax_errno"));
    assert!(manager.contains("use anyhow::{Context, Result};"));
    assert!(config.contains("AxVmError::Boot"));
    assert!(shell.contains("{err:#}"));
    assert!(!shell.contains("Err(\"Failed to boot VM\")"));
}
