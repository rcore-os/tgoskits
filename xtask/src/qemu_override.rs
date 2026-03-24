// Copyright 2026 The tgoskits Team
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

use std::{
    env::current_dir,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use axbuild::{
    Arch,
    arceos::{RunScope, resolve_package_app_dir},
};

#[derive(Debug, Clone)]
pub struct RuntimeQemuOverride {
    pub blk: bool,
    pub disk_img: Option<String>,
    pub net: bool,
    pub net_dev: Option<String>,
    pub graphic: bool,
    pub accel: bool,
}

pub fn resolve_qemu_search_dir(
    manifest_dir: &Path,
    package: &str,
    run_scope: RunScope,
) -> Result<PathBuf> {
    match run_scope {
        RunScope::Default => Ok(manifest_dir.to_path_buf()),
        RunScope::PackageRoot => {
            let relative = resolve_package_app_dir(manifest_dir, package)?;
            Ok(manifest_dir.join(relative))
        }
        RunScope::StarryOsRoot => Ok(manifest_dir.join("os/StarryOS")),
    }
}

pub fn resolve_external_qemu_config_path(
    manifest_dir: &Path,
    search_dir: &Path,
    arch: Arch,
) -> Result<PathBuf> {
    let candidates = [
        format!("qemu-{}.toml", arch.to_qemu_arch()),
        format!(".qemu-{}.toml", arch.to_qemu_arch()),
        "qemu.toml".to_string(),
        ".qemu.toml".to_string(),
    ];
    for filename in &candidates {
        let path = search_dir.join(filename);
        if path.exists() {
            return Ok(path);
        }
    }
    for filename in &candidates {
        let path = manifest_dir.join(filename);
        if path.exists() {
            return Ok(path);
        }
    }
    anyhow::bail!(
        "no external qemu config found in {} or {}",
        search_dir.display(),
        manifest_dir.display()
    );
}

pub fn write_qemu_override_file(
    base_qemu_path: &Path,
    runtime: &RuntimeQemuOverride,
    success_regex: &[String],
    fail_regex: &[String],
    arch: Arch,
) -> Result<PathBuf> {
    let base = fs::read_to_string(base_qemu_path)
        .with_context(|| format!("failed to read {}", base_qemu_path.display()))?;
    let mut value: toml::Value = toml::from_str(&base)
        .with_context(|| format!("failed to parse {}", base_qemu_path.display()))?;
    let table = value
        .as_table_mut()
        .context("qemu config root must be a TOML table")?;

    let current_args = table
        .get("args")
        .and_then(|value| value.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut new_args = strip_runtime_overrides(&current_args);
    apply_qemu_runtime_overrides(&mut new_args, runtime, arch);
    table.insert(
        "args".to_string(),
        toml::Value::Array(new_args.into_iter().map(toml::Value::String).collect()),
    );
    table.insert(
        "success_regex".to_string(),
        toml::Value::Array(
            success_regex
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );
    table.insert(
        "fail_regex".to_string(),
        toml::Value::Array(
            fail_regex
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );
    if !table.contains_key("to_bin") {
        table.insert("to_bin".to_string(), toml::Value::Boolean(true));
    }
    if !table.contains_key("uefi") {
        table.insert("uefi".to_string(), toml::Value::Boolean(false));
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_millis();
    let output_path = current_dir()?
        .join("target")
        .join("xtask")
        .join(format!("qemu-override-{}-{stamp}.toml", std::process::id()));
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&output_path, toml::to_string_pretty(&value)?)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(output_path)
}

fn strip_runtime_overrides(args: &[String]) -> Vec<String> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-nographic" {
            i += 1;
            continue;
        }
        if matches!(
            arg.as_str(),
            "-device" | "-drive" | "-netdev" | "-display" | "-serial" | "-accel"
        ) {
            i += 2;
            continue;
        }
        stripped.push(arg.clone());
        i += 1;
    }
    stripped
}

fn apply_qemu_runtime_overrides(args: &mut Vec<String>, run: &RuntimeQemuOverride, arch: Arch) {
    if run.blk {
        args.push("-device".to_string());
        args.push("virtio-blk-pci,drive=disk0".to_string());
        if let Some(disk_img) = &run.disk_img {
            args.push("-drive".to_string());
            args.push(format!("id=disk0,if=none,format=raw,file={disk_img}"));
        }
    }

    if run.net {
        args.push("-device".to_string());
        args.push("virtio-net-pci,netdev=net0".to_string());
        args.push("-netdev".to_string());
        let netdev = match run.net_dev.as_deref().unwrap_or("user") {
            "tap" => "tap,id=net0,script=no",
            "bridge" => "bridge,id=net0,br=virbr0",
            _ => "user,id=net0,hostfwd=tcp::5555-:5555",
        };
        args.push(netdev.to_string());
    }

    if run.graphic {
        args.push("-device".to_string());
        args.push("virtio-gpu-pci".to_string());
        args.push("-display".to_string());
        args.push("gtk".to_string());
    } else {
        args.push("-nographic".to_string());
        args.push("-serial".to_string());
        args.push("mon:stdio".to_string());
    }

    if run.accel {
        match arch {
            Arch::X86_64 => {
                args.push("-accel".to_string());
                args.push("kvm".to_string());
            }
            Arch::AArch64 => {
                args.push("-accel".to_string());
                args.push("hvf".to_string());
            }
            Arch::RiscV64 | Arch::LoongArch64 => {}
        }
    }
}
