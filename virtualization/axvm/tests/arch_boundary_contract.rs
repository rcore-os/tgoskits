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
fn arch_root_contains_only_architecture_directories_and_dispatch_page() {
    let arch_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/arch");
    let mut unexpected_entries = std::fs::read_dir(&arch_root)
        .expect("AxVM architecture directory must be readable")
        .map(|entry| entry.expect("AxVM architecture entry must be readable"))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            !matches!(
                name.as_str(),
                "aarch64" | "loongarch64" | "riscv64" | "x86_64" | "mod.rs"
            )
        })
        .collect::<Vec<_>>();
    unexpected_entries.sort();

    assert!(
        unexpected_entries.is_empty(),
        "arch root must contain only architecture directories and the dispatch page; found: {}",
        unexpected_entries.join(", ")
    );
}

#[test]
fn arch_dispatch_page_does_not_own_common_implementations() {
    let dispatch = include_str!("../src/arch/mod.rs");

    for forbidden in [
        "#[path",
        "trait ArchOps",
        "struct MmioReadExit",
        "fn handle_mmio_read",
        "fn default_vcpu_affinities",
    ] {
        assert!(
            !dispatch.contains(forbidden),
            "arch/mod.rs must only select and export the current architecture: {forbidden}"
        );
    }
}

#[test]
fn common_domains_live_outside_architecture_directories() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    for relative_path in [
        "boot/fdt/mod.rs",
        "boot/images/mod.rs",
        "host/arceos.rs",
        "npt.rs",
    ] {
        assert!(
            source_root.join(relative_path).is_file(),
            "common AxVM domain must use its canonical source path: {relative_path}"
        );
    }
}

#[test]
fn common_modules_do_not_include_architecture_sources() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    find_source_files(&source_root, &mut |path, source| {
        if !path.starts_with(source_root.join("arch"))
            && source.contains("#[path")
            && source.contains("arch/")
        {
            violations.push(source_relative_path(&source_root, path));
        }
    });

    assert!(
        violations.is_empty(),
        "common modules must not include implementations from src/arch: {}",
        violations.join(", ")
    );
}

#[test]
fn architecture_directories_only_select_their_own_target() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let arch_root = source_root.join("arch");
    let architectures = ["aarch64", "loongarch64", "riscv64", "x86_64"];
    let mut violations = Vec::new();

    for architecture in architectures {
        find_source_files(&arch_root.join(architecture), &mut |path, source| {
            for other_architecture in architectures {
                if other_architecture != architecture
                    && source.contains(&format!("target_arch = \"{other_architecture}\""))
                {
                    violations.push(format!(
                        "{} selects {other_architecture}",
                        source_relative_path(&source_root, path)
                    ));
                }
            }
        });
    }

    assert!(
        violations.is_empty(),
        "an architecture directory must not select another target: {}",
        violations.join(", ")
    );
}

#[test]
fn axvisor_vm_creation_uses_unified_guest_boot_facade() {
    let axvisor_config =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../os/axvisor/src/config.rs");
    let source = std::fs::read_to_string(&axvisor_config)
        .expect("Axvisor VM creation source must be readable");

    for legacy_call in [
        "handle_fdt_operations",
        "ImageLoader::new",
        "x86_linux_direct_boot_config",
        "DEFAULT_X86_BIOS_LOAD_GPA",
    ] {
        assert!(
            !source.contains(legacy_call),
            "Axvisor VM creation must use the unified AxVM boot facade: {legacy_call}"
        );
    }
}

#[test]
fn host_time_trait_only_exposes_common_clock_capabilities() {
    let host_traits = include_str!("../src/host/traits.rs");

    for architecture_specific_detail in ["CancelToken", "fn register_timer"] {
        assert!(
            !host_traits.contains(architecture_specific_detail),
            "HostTime must not expose architecture-specific timer details: \
             {architecture_specific_detail}"
        );
    }
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

fn find_source_files(directory: &std::path::Path, visit: &mut impl FnMut(&std::path::Path, &str)) {
    for entry in std::fs::read_dir(directory).expect("AxVM source directory must be readable") {
        let entry = entry.expect("AxVM source directory entry must be readable");
        let path = entry.path();
        if path.is_dir() {
            find_source_files(&path, visit);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let source = std::fs::read_to_string(&path).expect("AxVM source file must be readable");
            visit(&path, &source);
        }
    }
}

fn source_relative_path(source_root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(source_root)
        .expect("source path must be below src")
        .display()
        .to_string()
}
