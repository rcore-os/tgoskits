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
fn vm_core_does_not_handle_arch_local_exits() {
    let vm_rs = include_str!("../src/vm.rs");

    for forbidden in [
        "CurrentArch",
        "ArchOps",
        "CurrentArch::handle_vcpu_exit",
        "VcpuRunAction",
        "HostInterrupt",
    ] {
        assert!(
            !vm_rs.contains(forbidden),
            "vm.rs must not contain architecture-local exit handling detail: {forbidden}"
        );
    }
}

#[test]
fn runtime_vcpu_loop_only_consumes_scheduler_actions() {
    let runtime_vcpus_rs = include_str!("../src/runtime/vcpus.rs");

    for forbidden in [
        "VcpuRunAction::Continue",
        "VcpuRunAction::HostInterrupt",
        "HostInterrupt",
    ] {
        assert!(
            !runtime_vcpus_rs.contains(forbidden),
            "runtime/vcpus.rs must not match architecture-local exit action: {forbidden}"
        );
    }
}

#[test]
fn production_sources_keep_architecture_cfg_inside_arch_module() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let violations = find_target_arch_cfg_outside_arch(&source_root, &source_root);

    assert!(
        violations.is_empty(),
        "target_arch must stay inside src/arch; found: {}",
        violations.join(", ")
    );
}

#[test]
fn arch_root_contains_only_architectures_and_shared_dispatch_modules() {
    let arch_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/arch");
    let mut unexpected_entries = std::fs::read_dir(&arch_root)
        .expect("AxVM architecture directory must be readable")
        .map(|entry| entry.expect("AxVM architecture entry must be readable"))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            !matches!(
                name.as_str(),
                "aarch64" | "loongarch64" | "riscv64" | "x86_64" | "mod.rs" | "npt.rs"
            )
        })
        .collect::<Vec<_>>();
    unexpected_entries.sort();

    assert!(
        unexpected_entries.is_empty(),
        "arch root must contain only architecture directories and shared dispatch modules; found: \
         {}",
        unexpected_entries.join(", ")
    );
}

fn find_target_arch_cfg_outside_arch(
    source_root: &std::path::Path,
    directory: &std::path::Path,
) -> Vec<String> {
    let mut violations = Vec::new();
    for entry in std::fs::read_dir(directory).expect("AxVM source directory must be readable") {
        let entry = entry.expect("AxVM source directory entry must be readable");
        let path = entry.path();
        if path.is_dir() {
            if path != source_root.join("arch") {
                violations.extend(find_target_arch_cfg_outside_arch(source_root, &path));
            }
            continue;
        }

        if path.extension().is_some_and(|extension| extension == "rs")
            && std::fs::read_to_string(&path)
                .expect("AxVM source file must be readable")
                .contains("target_arch")
        {
            violations.push(
                path.strip_prefix(source_root)
                    .expect("source path must be below src")
                    .display()
                    .to_string(),
            );
        }
    }
    violations
}
