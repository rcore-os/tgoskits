use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail};

pub(super) struct ScopedEnvVar {
    key: &'static str,
    previous: Option<OsString>,
    active: bool,
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        match &self.previous {
            Some(value) => {
                // SAFETY: qperf runs this CLI flow serially and restores the process
                // environment before returning to the caller.
                unsafe { env::set_var(self.key, value) };
            }
            None => {
                // SAFETY: qperf runs this CLI flow serially and restores the process
                // environment before returning to the caller.
                unsafe { env::remove_var(self.key) };
            }
        }
    }
}

pub(super) fn set_env_if_missing(
    key: &'static str,
    value: OsString,
) -> anyhow::Result<ScopedEnvVar> {
    let previous = env::var_os(key);
    if previous.as_ref().is_some_and(|value| !value.is_empty()) {
        return Ok(ScopedEnvVar {
            key,
            previous,
            active: false,
        });
    }
    let path = PathBuf::from(&value);
    fs::create_dir_all(&path)
        .with_context(|| format!("failed to create {key} directory {}", path.display()))?;
    // SAFETY: qperf runs this CLI flow serially before spawning worker threads that depend on
    // axbuild paths.
    unsafe { env::set_var(key, &value) };
    Ok(ScopedEnvVar {
        key,
        previous,
        active: true,
    })
}

fn set_env_var(key: &'static str, value: OsString) -> ScopedEnvVar {
    let previous = env::var_os(key);
    // SAFETY: qperf runs this CLI flow serially before spawning child processes
    // that depend on the adjusted environment.
    unsafe { env::set_var(key, &value) };
    ScopedEnvVar {
        key,
        previous,
        active: true,
    }
}

pub(super) fn prepare_cross_c_compiler_fallback(
    work_dir: &Path,
    arch: &str,
) -> anyhow::Result<Vec<ScopedEnvVar>> {
    let (compiler, zig_target, zig_include_dirs) = match arch {
        "riscv64" => (
            "riscv64-linux-musl-gcc",
            "riscv64-linux-musl",
            [
                "libc/include/riscv64-linux-musl",
                "libc/include/generic-musl",
                "libc/include/riscv-linux-any",
                "libc/include/any-linux-any",
                "include",
            ],
        ),
        _ => return Ok(Vec::new()),
    };

    if cross_c_compiler_works(compiler) {
        return Ok(Vec::new());
    }

    let zig = find_executable("zig").ok_or_else(|| {
        anyhow::anyhow!(
            "StarryOS qperf {arch} build requires `{compiler}` in PATH; install a riscv64 musl C \
             compiler or install `zig` so starry perf can create a local cross-cc wrapper"
        )
    })?;
    let wrapper_root = work_dir.join("cross-cc");
    let bin_dir = wrapper_root.join("bin");
    let sysroot = wrapper_root.join(format!("{zig_target}-sysroot"));
    let zig_lib = zig_lib_dir(&zig)?;
    prepare_zig_sysroot(&sysroot, &zig_lib, &zig_include_dirs)?;
    fs::create_dir_all(&bin_dir).with_context(|| {
        format!(
            "failed to create cross-cc bin directory {}",
            bin_dir.display()
        )
    })?;
    let wrapper = bin_dir.join(compiler);
    write_zig_cc_wrapper(&wrapper, &zig, zig_target, &sysroot)?;
    let ar_wrapper = bin_dir.join("riscv64-linux-musl-ar");
    let ranlib_wrapper = bin_dir.join("riscv64-linux-musl-ranlib");
    write_zig_tool_wrapper(&ar_wrapper, &zig, "ar")?;
    write_zig_tool_wrapper(&ranlib_wrapper, &zig, "ranlib")?;

    let mut paths = vec![bin_dir];
    if let Some(current) = env::var_os("PATH") {
        paths.extend(env::split_paths(&current));
    }
    let path =
        env::join_paths(paths).context("failed to prepend qperf cross-cc wrapper to PATH")?;
    eprintln!(
        "qperf: `{compiler}` missing or unusable; using Zig cross C wrapper at {}",
        wrapper.display()
    );
    let bindgen_args = bindgen_extra_clang_args(&zig_lib, &zig_include_dirs, zig_target);
    Ok(vec![
        set_env_var("PATH", path),
        set_env_var(
            "BINDGEN_EXTRA_CLANG_ARGS_riscv64-unknown-linux-musl",
            bindgen_args.clone().into(),
        ),
        set_env_var(
            "BINDGEN_EXTRA_CLANG_ARGS_riscv64_unknown_linux_musl",
            bindgen_args.clone().into(),
        ),
        set_env_var(
            "BINDGEN_EXTRA_CLANG_ARGS",
            append_env_words("BINDGEN_EXTRA_CLANG_ARGS", &bindgen_args),
        ),
        set_env_var(
            "AR_riscv64gc_unknown_none_elf",
            ar_wrapper.as_os_str().to_os_string(),
        ),
        set_env_var(
            "AR_riscv64gc-unknown-none-elf",
            ar_wrapper.as_os_str().to_os_string(),
        ),
        set_env_var(
            "RANLIB_riscv64gc_unknown_none_elf",
            ranlib_wrapper.as_os_str().to_os_string(),
        ),
        set_env_var(
            "RANLIB_riscv64gc-unknown-none-elf",
            ranlib_wrapper.as_os_str().to_os_string(),
        ),
    ])
}

fn cross_c_compiler_works(compiler: &str) -> bool {
    if !compiler_command_succeeds(compiler, &["-print-sysroot"]) {
        return false;
    }

    compiler_command_succeeds(compiler, &["-E", "-x", "c", "-"])
}

fn compiler_command_succeeds(compiler: &str, args: &[&str]) -> bool {
    Command::new(compiler)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn zig_lib_dir(zig: &Path) -> anyhow::Result<PathBuf> {
    let output = Command::new(zig)
        .arg("env")
        .output()
        .with_context(|| format!("failed to run {} env", zig.display()))?;
    if !output.status.success() {
        bail!("{} env failed with {}", zig.display(), output.status);
    }
    let text = String::from_utf8(output.stdout).context("zig env output was not UTF-8")?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(lib_dir) = value.get("lib_dir").and_then(|value| value.as_str())
    {
        return Ok(PathBuf::from(lib_dir));
    }
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix(".lib_dir = ") {
            let lib_dir = value.trim_end_matches(',').trim().trim_matches('"');
            if !lib_dir.is_empty() {
                return Ok(PathBuf::from(lib_dir));
            }
        }
    }
    bail!("zig env did not report lib_dir")
}

fn prepare_zig_sysroot(
    sysroot: &Path,
    zig_lib: &Path,
    include_dirs: &[&str],
) -> anyhow::Result<()> {
    let include = sysroot.join("include");
    fs::create_dir_all(&include)
        .with_context(|| format!("failed to create Zig sysroot include {}", include.display()))?;
    for dir in include_dirs {
        let source = zig_lib.join(dir);
        if source.exists() {
            merge_include_dir(&source, &include)?;
        }
    }
    Ok(())
}

fn bindgen_extra_clang_args(zig_lib: &Path, include_dirs: &[&str], target: &str) -> String {
    let mut args = vec![format!("--target={target}")];
    for dir in include_dirs {
        let include = zig_lib.join(dir);
        if include.exists() {
            args.push("-isystem".to_string());
            args.push(include.display().to_string());
        }
    }
    args.join(" ")
}

fn append_env_words(key: &str, prefix: &str) -> OsString {
    match env::var_os(key) {
        Some(current) if !current.is_empty() => {
            let mut value = OsString::from(prefix);
            value.push(" ");
            value.push(current);
            value
        }
        _ => OsString::from(prefix),
    }
}

fn merge_include_dir(source: &Path, dest: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(source)
        .with_context(|| format!("failed to read Zig include directory {}", source.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to read Zig include entry {}", source.display()))?;
        let source_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        let metadata = entry.metadata().with_context(|| {
            format!("failed to stat Zig include entry {}", source_path.display())
        })?;
        if metadata.is_dir() {
            fs::create_dir_all(&dest_path).with_context(|| {
                format!(
                    "failed to create Zig include directory {}",
                    dest_path.display()
                )
            })?;
            merge_include_dir(&source_path, &dest_path)?;
        } else if !dest_path.exists() {
            symlink_file(&source_path, &dest_path)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink_file(source: &Path, dest: &Path) -> anyhow::Result<()> {
    std::os::unix::fs::symlink(source, dest).with_context(|| {
        format!(
            "failed to symlink Zig header {} -> {}",
            dest.display(),
            source.display()
        )
    })
}

#[cfg(not(unix))]
fn symlink_file(source: &Path, dest: &Path) -> anyhow::Result<()> {
    fs::copy(source, dest).with_context(|| {
        format!(
            "failed to copy Zig header {} -> {}",
            source.display(),
            dest.display()
        )
    })?;
    Ok(())
}

fn write_zig_cc_wrapper(
    wrapper: &Path,
    zig: &Path,
    target: &str,
    sysroot: &Path,
) -> anyhow::Result<()> {
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
zig_bin="${{ZIG:-{zig}}}"
target="{target}"
sysroot="{sysroot}"
if [[ "$#" -eq 1 && "$1" == "-print-sysroot" ]]; then
  printf '%s\n' "$sysroot"
  exit 0
fi
args=()
skip_next=0
for arg in "$@"; do
  if [[ "$skip_next" -eq 1 ]]; then
    skip_next=0
    continue
  fi
  case "$arg" in
    --target)
      skip_next=1
      ;;
    --target=riscv64|--target=riscv64gc|--target=riscv64-unknown-none-elf|--target=riscv64gc-unknown-none-elf|-march=rv64gc|-mabi=lp64d)
      ;;
    *)
      args+=("$arg")
      ;;
  esac
done
exec "$zig_bin" cc -target "$target" "${{args[@]}}"
"#,
        zig = zig.display(),
        target = target,
        sysroot = sysroot.display(),
    );
    fs::write(wrapper, script).with_context(|| {
        format!(
            "failed to write Zig C compiler wrapper {}",
            wrapper.display()
        )
    })?;
    set_executable(wrapper)?;
    Ok(())
}

fn write_zig_tool_wrapper(wrapper: &Path, zig: &Path, tool: &str) -> anyhow::Result<()> {
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
zig_bin="${{ZIG:-{zig}}}"
exec "$zig_bin" {tool} "$@"
"#,
        zig = zig.display(),
        tool = tool,
    );
    fs::write(wrapper, script)
        .with_context(|| format!("failed to write Zig tool wrapper {}", wrapper.display()))?;
    set_executable(wrapper)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to chmod +x {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

pub(super) fn find_executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 && path.is_file() {
        return Some(path.to_path_buf());
    }
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::cross_c_compiler_works;

    #[cfg(unix)]
    #[test]
    fn cross_c_compiler_probe_does_not_require_clang_target_arg() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let compiler = temp.path().join("riscv64-linux-musl-gcc");
        fs::write(
            &compiler,
            r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    --target|--target=*)
      exit 42
      ;;
  esac
done

if [ "$1" = "-print-sysroot" ]; then
  printf '%s
' /fake/sysroot
  exit 0
fi

case " $* " in
  *" -E -x c - "*)
    cat >/dev/null
    exit 0
    ;;
esac

exit 1
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&compiler).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&compiler, permissions).unwrap();

        assert!(cross_c_compiler_works(compiler.to_str().unwrap()));
    }
}
