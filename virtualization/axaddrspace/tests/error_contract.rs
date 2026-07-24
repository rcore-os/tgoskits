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

use ax_memory_set::MappingError;
use axaddrspace::AddrSpaceError;

#[test]
fn mapping_errors_convert_with_from() {
    assert_eq!(
        AddrSpaceError::from(MappingError::AlreadyExists),
        AddrSpaceError::MappingConflict
    );
    assert_eq!(
        AddrSpaceError::from(MappingError::InvalidParam),
        AddrSpaceError::InvalidMapping
    );
    assert_eq!(
        AddrSpaceError::from(MappingError::BadState),
        AddrSpaceError::MappingState
    );
    assert_eq!(
        AddrSpaceError::from(MappingError::NoMemory),
        AddrSpaceError::NoMemory
    );
}

#[test]
fn manifest_and_sources_do_not_reference_axerrno() {
    let manifest = include_str!("../Cargo.toml");
    assert!(!manifest.contains("ax-errno"));
    assert!(manifest.contains("thiserror = { workspace = true }"));

    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![source_root];
    let mut violations = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && std::fs::read_to_string(&path).unwrap().contains("ax_errno")
            {
                violations.push(path);
            }
        }
    }
    assert!(violations.is_empty(), "ax_errno references: {violations:?}");
}
