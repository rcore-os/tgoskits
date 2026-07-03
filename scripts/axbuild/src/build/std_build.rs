use super::*;

pub(super) struct StdBuildTarget {
    pub(super) target_name: String,
    pub(super) target: String,
    pub(super) cargo_args: Vec<String>,
    pub(super) env: HashMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct StdCargoConfig {
    unstable: StdCargoUnstableConfig,
    profile: StdCargoProfileConfig,
    target: HashMap<String, StdCargoTargetConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct StdCargoUnstableConfig {
    build_std: Vec<&'static str>,
    build_std_features: Vec<&'static str>,
}

#[derive(Serialize)]
struct StdCargoProfileConfig {
    release: StdCargoReleaseProfile,
}

#[derive(Serialize)]
struct StdCargoReleaseProfile {
    lto: bool,
    panic: &'static str,
}

#[derive(Serialize)]
struct StdCargoTargetConfig {
    linker: String,
    rustflags: Vec<String>,
}

impl StdCargoConfig {
    fn new(target: &str, linker: &Path, extra_rustflags: &[String]) -> Self {
        Self {
            unstable: StdCargoUnstableConfig {
                build_std: vec!["std", "panic_abort"],
                build_std_features: Vec::new(),
            },
            profile: StdCargoProfileConfig {
                release: StdCargoReleaseProfile {
                    lto: false,
                    panic: "abort",
                },
            },
            target: HashMap::from([(
                target.to_string(),
                StdCargoTargetConfig {
                    linker: linker.display().to_string(),
                    rustflags: extra_rustflags.to_vec(),
                },
            )]),
        }
    }
}

pub(super) fn std_build_target_for(target: &str) -> anyhow::Result<StdBuildTarget> {
    let (target_name, tool_prefix) = if target.starts_with("x86_64-") {
        ("x86_64-unknown-linux-musl", "x86_64-linux-musl")
    } else if target.starts_with("aarch64-") {
        ("aarch64-unknown-linux-musl", "aarch64-linux-musl")
    } else if target.starts_with("riscv64") {
        ("riscv64gc-unknown-linux-musl", "riscv64-linux-musl")
    } else if target.starts_with("loongarch64-") {
        ("loongarch64-unknown-linux-musl", "loongarch64-linux-musl")
    } else {
        bail!("unsupported ArceOS std target triple `{target}`");
    };

    let mut env = HashMap::new();
    env.insert(
        "CARGO_UNSTABLE_JSON_TARGET_SPEC".to_string(),
        "true".to_string(),
    );
    env.extend(std_c_toolchain_env(target_name, tool_prefix));

    Ok(StdBuildTarget {
        target_name: target_name.to_string(),
        target: std_target_json_path(target_name).display().to_string(),
        cargo_args: vec!["-Z".to_string(), "json-target-spec".to_string()],
        env,
    })
}

pub(super) fn std_c_toolchain_env(target_name: &str, tool_prefix: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let target_env = target_name.replace('-', "_");
    let cc = format!("{tool_prefix}-cc");
    let ar = format!("{tool_prefix}-ar");
    let c_flags = std_c_target_flags(target_name).join(" ");
    env.insert(format!("CC_{target_env}"), cc.clone());
    env.insert(format!("AR_{target_env}"), ar);
    if !c_flags.is_empty() {
        env.insert(format!("CFLAGS_{target_env}"), c_flags.clone());
        env.insert(format!("CXXFLAGS_{target_env}"), c_flags.clone());
    }

    if let Some(sysroot) = musl_toolchain_sysroot(&cc) {
        let mut bindgen_args = vec![
            format!("--target={tool_prefix}"),
            format!("--sysroot={sysroot}"),
        ];
        bindgen_args.extend(musl_toolchain_bindgen_args(&cc, &sysroot, tool_prefix));
        bindgen_args.extend(
            std_c_target_flags(target_name)
                .into_iter()
                .map(str::to_string),
        );
        env.insert(
            format!("BINDGEN_EXTRA_CLANG_ARGS_{target_env}"),
            bindgen_args.join(" "),
        );
    }

    env
}

pub(super) fn std_c_target_flags(target_name: &str) -> Vec<&'static str> {
    if target_name.starts_with("x86_64-") {
        vec![
            "-mno-mmx",
            "-mno-sse",
            "-mno-sse2",
            "-mno-sse3",
            "-mno-ssse3",
            "-mno-sse4.1",
            "-mno-sse4.2",
            "-mno-avx",
            "-mno-avx2",
            "-msoft-float",
        ]
    } else if target_name.starts_with("aarch64-") {
        vec!["-mgeneral-regs-only"]
    } else if target_name.starts_with("riscv64") {
        vec!["-march=rv64gc", "-mabi=lp64d", "-mcmodel=medany"]
    } else if target_name.starts_with("loongarch64-") {
        vec!["-mabi=lp64s", "-msoft-float"]
    } else {
        Vec::new()
    }
}

pub(super) fn musl_toolchain_sysroot(cc: &str) -> Option<String> {
    let output = std::process::Command::new(cc)
        .arg("-print-sysroot")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sysroot = String::from_utf8(output.stdout).ok()?;
    let sysroot = sysroot.trim();
    (!sysroot.is_empty()).then(|| sysroot.to_string())
}

pub(super) fn musl_toolchain_bindgen_args(
    cc: &str,
    sysroot: &str,
    tool_prefix: &str,
) -> Vec<String> {
    let Some(toolchain_root) = musl_toolchain_root(cc, sysroot) else {
        return Vec::new();
    };

    let mut args = vec![format!("--gcc-toolchain={}", toolchain_root.display())];

    let include = Path::new(sysroot).join("include");
    if include.is_dir() {
        args.push("-isystem".to_string());
        args.push(include.display().to_string());
    }

    if let Some(gcc_include) = musl_gcc_include_dir(&toolchain_root, tool_prefix) {
        args.push("-isystem".to_string());
        args.push(gcc_include.display().to_string());
    }

    args
}

fn musl_toolchain_root(cc: &str, sysroot: &str) -> Option<PathBuf> {
    let cc_path = command_path(cc)?;
    let bin_dir = cc_path.parent()?;
    let root = bin_dir.parent()?;
    let sysroot_path = fs::canonicalize(sysroot).ok()?;
    let root_path = fs::canonicalize(root).ok()?;

    sysroot_path
        .starts_with(&root_path)
        .then(|| root.to_path_buf())
}

fn command_path(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.exists().then(|| path.to_path_buf());
    }

    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(Path::new)
        .map(|dir| dir.join(command))
        .find(|path| path.exists())
}

fn musl_gcc_include_dir(toolchain_root: &Path, tool_prefix: &str) -> Option<PathBuf> {
    let gcc_dir = toolchain_root.join("lib/gcc").join(tool_prefix);
    fs::read_dir(gcc_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("include"))
        .find(|path| path.is_dir())
}

pub(super) fn std_target_json_path(target: &str) -> PathBuf {
    let path = Path::new(TARGET_JSON_ROOT).join(STD_TARGET_DIR);
    path.join(PIE_TARGET_DIR).join(format!("{target}.json"))
}

pub(crate) fn prepare_std_build_env(
    envs: &mut HashMap<String, String>,
    target: &str,
    metadata: &Metadata,
) -> anyhow::Result<()> {
    prepare_std_build_env_for_package(envs, AXSTD_STD_PACKAGE, target, &[], metadata)
}

pub(super) fn prepare_std_build_env_for_package(
    envs: &mut HashMap<String, String>,
    package: &str,
    target: &str,
    features: &[String],
    metadata: &Metadata,
) -> anyhow::Result<()> {
    envs.insert("AX_TARGET".to_string(), target.to_string());

    let _ = (package, features, metadata);
    Ok(())
}

pub(super) fn pass_std_build_nested_features(
    _envs: &mut HashMap<String, String>,
    features: &mut Vec<String>,
    app_features: &[String],
    axstd_features: &[String],
) {
    let mut cargo_features = Vec::new();

    for feature in features.drain(..) {
        let feature = normalize_std_feature(&feature);
        if is_removed_dynamic_platform_feature(&feature) {
            continue;
        }
        if matches!(feature.as_str(), "ax-std" | "ax-feat") {
            continue;
        }
        if is_log_level_feature(&feature) {
            continue;
        }
        if is_axstd_std_check_feature(&feature) {
            if std_feature_stays_on_app(&feature, app_features) {
                cargo_features.push(feature.clone());
            }
            let axstd_feature = axstd_feature_name(&feature);
            if axstd_feature_is_available(axstd_feature, axstd_features) {
                cargo_features.push(format!("ax-std/{axstd_feature}"));
            } else if feature.contains('/') {
                cargo_features.push(feature);
            }
        } else {
            cargo_features.push(feature);
        }
    }

    if axstd_feature_is_available("std-compat", axstd_features) {
        cargo_features.push("ax-std/std-compat".to_string());
    }

    cargo_features.sort();
    cargo_features.dedup();

    *features = cargo_features;
}

pub(super) fn inject_arceos_feature_for_std_build(
    features: &mut Vec<String>,
    app_features: &[String],
) {
    if app_features.iter().any(|feature| feature == "arceos")
        && !features.iter().any(|feature| feature == "arceos")
    {
        features.push("arceos".to_string());
    }
}

pub(super) fn axstd_feature_name(feature: &str) -> &str {
    feature
        .strip_prefix("ax-hal/")
        .or_else(|| feature.strip_prefix("ax-driver/"))
        .unwrap_or(feature)
}

pub(super) fn package_feature_names(
    package: &str,
    metadata: &Metadata,
) -> anyhow::Result<Vec<String>> {
    Ok(workspace_package(metadata, package)?
        .features
        .keys()
        .cloned()
        .collect())
}

pub(super) fn axstd_feature_is_available(feature: &str, axstd_features: &[String]) -> bool {
    axstd_features
        .iter()
        .any(|axstd_feature| axstd_feature == feature)
}

pub(super) fn std_cargo_config_path(
    target: &str,
    linker: &Path,
    extra_rustflags: &[String],
) -> anyhow::Result<PathBuf> {
    let path = std_build_dir()?.join(format!("config-{target}-dynamic.toml"));
    let config = toml::to_string_pretty(&StdCargoConfig::new(target, linker, extra_rustflags))?;
    write_if_changed(&path, &config)?;
    Ok(path)
}

pub(super) fn std_fake_lib_dir(target: &str) -> anyhow::Result<PathBuf> {
    let dir = axbuild_tmp_dir(&crate::context::workspace_root_path()?)
        .join("std-libs")
        .join(target)
        .join("release");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create std fake lib dir {}", dir.display()))?;
    Ok(dir)
}

pub(super) fn std_linker_wrapper_path(
    target: &str,
    fake_lib_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let path = std_build_dir()?.join(format!("linker-{target}-dynamic.sh"));
    write_if_changed(&path, &std_linker_wrapper_script(target, fake_lib_dir)?)?;
    set_executable(&path)?;
    Ok(path)
}

pub(super) fn std_fake_lib_prebuild_script_path(
    target_name: &str,
    fake_lib_dir: &Path,
    envs: &HashMap<String, String>,
) -> anyhow::Result<PathBuf> {
    let contents = std_fake_lib_prebuild_script(target_name, fake_lib_dir, envs);
    let hash = short_content_hash(&contents);
    let path = std_build_dir()?
        .join("prebuild")
        .join(format!("prebuild-{target_name}-{hash}.sh"));
    write_if_changed(&path, &contents)?;
    set_executable(&path)?;
    Ok(path)
}

pub(super) fn std_fake_lib_prebuild_script(
    target_name: &str,
    fake_lib_dir: &Path,
    envs: &HashMap<String, String>,
) -> String {
    let mut env_exports = Vec::new();
    let mut sorted_envs: Vec<_> = envs.iter().collect();
    sorted_envs.sort_by_key(|(key, _)| *key);
    for (key, value) in sorted_envs {
        if key
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
        {
            env_exports.push(format!("export {key}={}", shell_single_quote(value)));
        }
    }

    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

{}

target_name={}
fake_dir={}

archive_tool() {{
    if command -v rust-ar >/dev/null 2>&1; then
        command -v rust-ar
        return
    fi

    local sysroot_llvm_ar
    sysroot_llvm_ar="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/llvm-ar"
    if [[ -x "$sysroot_llvm_ar" ]]; then
        printf '%s\n' "$sysroot_llvm_ar"
        return
    fi

    if command -v llvm-ar >/dev/null 2>&1; then
        command -v llvm-ar
        return
    fi

    if command -v ar >/dev/null 2>&1; then
        command -v ar
        return
    fi

    echo "failed to find archive tool; tried rust-ar, rust toolchain llvm-ar, llvm-ar, ar" >&2
    exit 127
}}

create_empty_archive() {{
    local archive="$1"
    rm -f "$archive"
    "$(archive_tool)" crs "$archive"
}}

mkdir -p "$fake_dir"
create_empty_archive "$fake_dir/libc.a"
create_empty_archive "$fake_dir/libunwind.a"
"#,
        env_exports.join("\n"),
        shell_single_quote(target_name),
        shell_single_quote(&fake_lib_dir.display().to_string()),
    )
}

pub(super) fn std_linker_wrapper_script(
    target: &str,
    fake_lib_dir: &Path,
) -> anyhow::Result<String> {
    let machine = lld_machine_for_std_target(target)?;
    Ok(format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

fake_dir={}
target_name={}
lld_args=("-m" "{}")
link_search_dirs=()
archive_args=()

add_link_search_dir() {{
    local dir="$1"
    [[ -n "$dir" ]] && link_search_dirs+=("$dir")
}}

append_lld_arg() {{
    local arg="$1"
    case "$arg" in
        *.a|*.rlib)
            archive_args+=("$arg")
            ;;
        *)
            flush_archive_group
            lld_args+=("$arg")
            ;;
    esac
}}

flush_archive_group() {{
    if (( ${{#archive_args[@]}} == 0 )); then
        return
    fi
    lld_args+=("--start-group" "${{archive_args[@]}}" "--end-group")
    archive_args=()
}}

find_linker_script() {{
    local dir
    for dir in "${{link_search_dirs[@]}}"; do
        if [[ -f "$dir/linker.x" ]]; then
            printf '%s\n' "$dir/linker.x"
            return
        fi
    done
}}

add_arg() {{
    local arg="$1"
    if [[ "${{expect_link_search_dir:-0}}" == "1" ]]; then
        add_link_search_dir "$arg"
        append_lld_arg "$arg"
        expect_link_search_dir=0
        return
    fi

    if [[ "${{skip_next_lld_driver_arg:-0}}" == "1" ]]; then
        skip_next_lld_driver_arg=0
        return
    fi

    case "$arg" in
        */rcrt1.o|rcrt1.o|*/crt1.o|crt1.o|*/Scrt1.o|Scrt1.o|*/crti.o|crti.o|*/crtn.o|crtn.o|*/crtbegin*.o|crtbegin*.o|*/crtend*.o|crtend*.o)
            return
            ;;
        -L)
            append_lld_arg "$arg"
            expect_link_search_dir=1
            return
            ;;
        -L*)
            add_link_search_dir "${{arg#-L}}"
            append_lld_arg "$arg"
            return
            ;;
        -flavor)
            skip_next_lld_driver_arg=1
            return
            ;;
        -flavor=*|-T*)
            return
            ;;
        -lc)
            append_lld_arg "$fake_dir/libc.a"
            return
            ;;
        -lgcc_s|-lgcc)
            return
            ;;
        -lunwind)
            if [[ -f "$fake_dir/libunwind.a" ]]; then
                append_lld_arg "$fake_dir/libunwind.a"
            fi
            return
            ;;
        -static-pie)
            append_lld_arg "-pie"
            return
            ;;
        -static)
            return
            ;;
        -pie)
            append_lld_arg "-pie"
            return
            ;;
        -no-pie)
            return
            ;;
        -nostartfiles|-nodefaultlibs|-nostdlib|-m*)
            return
            ;;
        --eh-frame-hdr|-z|relro|norelro|now|noexecstack)
            return
            ;;
        -Wl,*)
            local IFS=,
            local parts=(${{arg#-Wl,}})
            local part
            for part in "${{parts[@]}}"; do
                [[ -n "$part" ]] && add_arg "$part"
            done
            return
            ;;
    esac
    append_lld_arg "$arg"
}}

for arg in "$@"; do
    add_arg "$arg"
done

linker_script="$(find_linker_script)"
if [[ -z "$linker_script" ]]; then
    echo "failed to find linker.x in current linker search dirs for $target_name" >&2
    exit 1
fi

flush_archive_group
lld_args+=("-L$fake_dir" "-T$linker_script")
exec rust-lld -flavor gnu "${{lld_args[@]}}"
"#,
        shell_single_quote(&fake_lib_dir.display().to_string()),
        shell_single_quote(target),
        machine,
    ))
}

pub(super) fn lld_machine_for_std_target(target: &str) -> anyhow::Result<&'static str> {
    if target.starts_with("x86_64-") {
        Ok("elf_x86_64")
    } else if target.starts_with("aarch64-") {
        Ok("aarch64elf")
    } else if target.starts_with("riscv64") {
        Ok("elf64lriscv")
    } else if target.starts_with("loongarch64-") {
        Ok("elf64loongarch")
    } else {
        bail!("unsupported ArceOS std linker target `{target}`")
    }
}

pub(super) fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn short_content_hash(contents: &str) -> String {
    let digest = Sha256::digest(contents.as_bytes());
    format!("{digest:x}").chars().take(16).collect()
}

pub(super) fn set_executable(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

pub(super) fn std_build_dir() -> anyhow::Result<PathBuf> {
    let dir = axbuild_tmp_dir(&crate::context::workspace_root_path()?).join("std");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create std build dir {}", dir.display()))?;
    Ok(dir)
}

pub(super) fn write_if_changed(path: &Path, contents: &str) -> anyhow::Result<()> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent dir {}", parent.display()))?;
    }
    let tmp_path = temporary_sibling_path(path);
    fs::write(&tmp_path, contents)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(path);
        fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "failed to replace {} with {} after initial rename error: {err}",
                path.display(),
                tmp_path.display()
            )
        })?;
    }
    Ok(())
}

pub(super) fn temporary_sibling_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "axbuild".into());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), nanos))
}
