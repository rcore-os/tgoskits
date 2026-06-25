use std::{fs, path::Path};

use anyhow::{Context, ensure};

use super::build;
use crate::context::ResolvedBuildRequest;

mod compile;
mod features;
mod flags;
mod libc;
mod link;
#[cfg(test)]
mod tests;
mod types;

use compile::{archive_static_lib, compile_dir_c_sources};
use features::{c_compiler_features, dynamic_pie_for_c_app, map_c_app_features};
use flags::{CFlagsInput, cflags, write_pthread_mutex_header};
use libc::{AX_LIBC_PACKAGE, build_axlibc_staticlib};
use link::{find_link_scripts, libgcc, link_c_app, platform_name};
use types::sanitize_name;
pub(crate) use types::{ArceosCBuildInput, ArceosCBuildOutput};

pub(crate) type ArceosCArtifactPaths = types::ArceosCArtifactPaths;

pub(crate) fn default_c_app_artifact_paths(
    workspace_root: &Path,
    app_name: &str,
) -> ArceosCArtifactPaths {
    types::default_c_app_artifact_paths(workspace_root, app_name)
}

pub(crate) fn build_c_app(
    workspace_root: &Path,
    request: &ResolvedBuildRequest,
    input: &ArceosCBuildInput,
) -> anyhow::Result<ArceosCBuildOutput> {
    let mut cargo = build::load_c_app_cargo_config(request)?;
    cargo.package = AX_LIBC_PACKAGE.to_string();
    cargo.target = request.target.clone();
    cargo.to_bin = false;
    cargo.features = map_c_app_features(&input.features, &cargo.features);
    let c_features = c_compiler_features(&cargo.features, &input.features);
    let dynamic_pie = dynamic_pie_for_c_app(&cargo.features);

    let mode = if request.debug { "debug" } else { "release" };
    let arch = request.arch.as_str();
    let arceos_dir = workspace_root.join("os/arceos");
    let axlibc_dir = arceos_dir.join("ulib/axlibc");
    let c_source_dir = axlibc_dir.join("c");
    let include_dir = axlibc_dir.join("include");
    let obj_root = input
        .target_dir
        .join("arceos-c")
        .join(sanitize_name(&input.app_name))
        .join(arch);
    let generated_include_dir = obj_root.join("include");
    let axlibc_obj_dir = obj_root.join("axlibc");
    let app_obj_dir = obj_root.join("app");
    fs::create_dir_all(&axlibc_obj_dir)
        .with_context(|| format!("failed to create {}", axlibc_obj_dir.display()))?;
    fs::create_dir_all(&app_obj_dir)
        .with_context(|| format!("failed to create {}", app_obj_dir.display()))?;
    fs::create_dir_all(&input.out_dir)
        .with_context(|| format!("failed to create {}", input.out_dir.display()))?;

    build_axlibc_staticlib(
        workspace_root,
        &cargo,
        &input.target_dir,
        request.debug,
        dynamic_pie,
    )?;
    write_pthread_mutex_header(&generated_include_dir, &cargo.features)?;
    let rust_lib = input
        .target_dir
        .join(&request.target)
        .join(mode)
        .join("libax_libc.a");
    ensure!(
        rust_lib.is_file(),
        "expected ax-libc static library at {}",
        rust_lib.display()
    );

    let platform = platform_name(&cargo.env);
    let link_scripts = find_link_scripts(
        &input.target_dir,
        &request.target,
        mode,
        &platform,
        &cargo.features,
    )
    .context("failed to locate ArceOS C app linker scripts")?;

    let cflags = cflags(CFlagsInput {
        workspace_root,
        arch,
        mode,
        generated_include_dir: &generated_include_dir,
        include_dir: &include_dir,
        features: &c_features,
        log: cargo.log,
        dynamic_pie,
    });
    let lib_objects =
        compile_dir_c_sources(&c_source_dir, &axlibc_obj_dir, &cflags, None, "axlibc")?;
    let app_objects = compile_dir_c_sources(&input.app_dir, &app_obj_dir, &cflags, None, "app")?;
    let libc = axlibc_obj_dir.join("libc.a");
    archive_static_lib(arch, &libc, &lib_objects)?;

    let elf_path = input
        .out_dir
        .join(format!("{}_{}.unstripped", input.app_name, platform));
    link_c_app(
        arch,
        &link_scripts,
        &elf_path,
        &rust_lib,
        &libc,
        &app_objects,
        libgcc(arch, &cargo.features)?,
    )?;

    Ok(ArceosCBuildOutput { elf_path })
}
