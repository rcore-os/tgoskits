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

use axvm_types::{VmBackendError, VmBackendResult};

#[test]
fn backend_errors_are_matchable_and_descriptive() {
    let result: VmBackendResult = Err(VmBackendError::ResourceBusy);

    assert_eq!(result, Err(VmBackendError::ResourceBusy));
    assert_eq!(
        VmBackendError::InvalidState.to_string(),
        "invalid virtualization backend state"
    );
    assert_eq!(
        VmBackendError::OutOfMemory.to_string(),
        "virtualization backend memory allocation failed"
    );
}

#[test]
fn backend_contract_uses_typed_errors() {
    let manifest = include_str!("../Cargo.toml");
    let crate_root = include_str!("../src/lib.rs");

    assert!(!manifest.contains("ax-errno"));
    assert!(manifest.contains("thiserror = { workspace = true }"));
    assert!(crate_root.contains("pub use error::{VmBackendError, VmBackendResult};"));
    assert!(!crate_root.contains("pub type AxVmError"));
    assert!(!crate_root.contains("pub type AxVmResult"));
}

#[test]
fn backend_sources_do_not_name_axerrno() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![source_root];
    let mut violations = Vec::new();

    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).expect("axvm-types source directory must be readable")
        {
            let entry = entry.expect("axvm-types source entry must be readable");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().is_some_and(|extension| extension == "rs") {
                let source =
                    std::fs::read_to_string(&path).expect("axvm-types source must be readable");
                if source.contains("ax_errno") {
                    violations.push(path);
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "axvm-types sources must not name ax_errno directly: {violations:?}"
    );
}
