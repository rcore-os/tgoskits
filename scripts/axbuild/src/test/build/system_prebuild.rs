use std::{
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

use self::monitor::monitor_prebuild_process;
use super::*;

mod monitor;

const PREBUILD_PASSED_MARKER: &str = "AXBUILD_SYSTEM_PREBUILD_PASSED";
const PREBUILD_FAILED_MARKER: &str = "AXBUILD_SYSTEM_PREBUILD_FAILED";
const PREBUILD_SHELL_PROMPT: &[u8] = b"~ # ";
const PREBUILD_TIMEOUT: Duration = Duration::from_secs(600);
const STAGING_MOUNT: &str = "/mnt/axbuild-staging";
const CASE_MOUNT: &str = "/mnt/axbuild-case";
const WORK_MOUNT: &str = "/mnt/axbuild-work";

pub(super) fn run_system_guest_prebuild(request: &GuestPrebuildRequest<'_>) -> anyhow::Result<()> {
    let spec = crate::context::linux_qemu_spec_for_arch_checked(request.arch)?;
    let qemu = find_host_binary_candidates(&[spec.binary]).with_context(|| {
        format!(
            "case `{}` requires {} because qemu-user is unavailable",
            request.case.display_name, spec.binary
        )
    })?;
    let kernel = request.layout.staging_root.join("guest/linux/linux-qemu");
    ensure!(
        kernel.is_file() && fs::metadata(&kernel)?.len() > 0,
        "staging root for `{}` is missing a usable Linux kernel at {}",
        request.case.display_name,
        kernel.display()
    );

    write_system_apk_wrapper(request.layout)?;
    let init_path = request.layout.run_dir.join("system-prebuild-init.sh");
    let init = system_prebuild_init_script(
        request.case,
        request.script,
        request.work_dir,
        request.layout,
        request.extra_envs.to_vec(),
        request.config,
    )?;
    fs::write(&init_path, init)
        .with_context(|| format!("failed to write {}", init_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&init_path, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to chmod {}", init_path.display()))?;
    }

    run_prebuild_qemu(
        qemu,
        spec,
        &kernel,
        request.case_rootfs,
        request.case,
        request.layout,
        &init_path,
    )
}

fn write_system_apk_wrapper(layout: &case_assets::CaseAssetLayout) -> anyhow::Result<()> {
    let wrapper_path = layout.command_wrapper_dir.join("apk");
    let body = format!(
        "exec /sbin/apk --root {staging} --repositories-file {staging}/etc/apk/repositories \
         --keys-dir {staging}/etc/apk/keys --cache-dir {cache} --update-cache --timeout 60 \
         --no-interactive --force-no-chroot --scripts=no \"$@\"\n",
        staging = shell_single_quote_value(STAGING_MOUNT),
        cache = shell_single_quote_value(&guest_work_path(layout, &layout.apk_cache_dir)?),
    );
    write_wrapper_script(&wrapper_path, &body)
}

fn system_prebuild_init_script(
    case: &TestQemuCase,
    prebuild_script: &Path,
    work_dir: &Path,
    layout: &case_assets::CaseAssetLayout,
    extra_script_envs: Vec<(String, String)>,
    config: &CaseAssetConfig,
) -> anyhow::Result<String> {
    let script = guest_case_path(case, prebuild_script)?;
    let current_dir = guest_case_path(case, work_dir)?;
    let wrapper_dir = guest_work_path(layout, &layout.command_wrapper_dir)?;
    let mut script_envs = vec![
        (
            config.script_env.staging_root.clone(),
            STAGING_MOUNT.to_string(),
        ),
        (config.script_env.case_dir.clone(), CASE_MOUNT.to_string()),
        (
            config.script_env.case_c_dir.clone(),
            format!("{CASE_MOUNT}/{CASE_C_DIR_NAME}"),
        ),
        (
            config.script_env.case_work_dir.clone(),
            WORK_MOUNT.to_string(),
        ),
        (
            config.script_env.case_build_dir.clone(),
            guest_work_path(layout, &layout.build_dir)?,
        ),
        (
            config.script_env.case_overlay_dir.clone(),
            guest_work_path(layout, &layout.overlay_dir)?,
        ),
    ];
    script_envs.extend(extra_script_envs);
    let host_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("host time is before the Unix epoch")?
        .as_secs();

    let mut exports = String::new();
    for (key, value) in script_envs {
        ensure!(
            valid_env_name(&key),
            "invalid prebuild environment variable `{key}`"
        );
        exports.push_str(&format!(
            "export {key}={}\n",
            shell_single_quote_value(&value)
        ));
    }

    Ok(format!(
        "#!/bin/sh\nset -eu\nfinish() {{\n\tstatus=$?\n\ttrap - EXIT\n\tsync\n\tif [ \"$status\" \
         -eq 0 ]; then\n\t\techo {passed}\n\telse\n\t\techo {failed} \
         status=\"$status\"\n\tfi\n\twhile :; do sleep 3600; done\n}}\ntrap finish EXIT\nexport \
         PATH={wrappers}:/usr/sbin:/usr/bin:/sbin:/bin\nmount -t proc proc /proc || true\nmount \
         -t sysfs sysfs /sys || true\nmount -t devtmpfs devtmpfs /dev || true\nip link set lo \
         up\ndate -s '@{host_epoch}' >/dev/null 2>&1 || true\nnetdev=\nfor interface in \
         /sys/class/net/*; do\n\t[ -e \"$interface/device\" ] || \
         continue\n\tnetdev=${{interface##*/}}\n\tbreak\ndone\nif [ -n \"$netdev\" ]; then\n\tip \
         link set \"$netdev\" up\n\tudhcpc -i \"$netdev\" -q -n || true\nfi\nmkdir -p {staging} \
         {case_mount} {work_mount}\nmount -t 9p -o trans=virtio,version=9p2000.L,msize=1048576 \
         axbuild-staging {staging}\nmount -t 9p -o trans=virtio,version=9p2000.L,msize=1048576 \
         axbuild-case {case_mount}\n{exports}cd {current_dir}\nsh -eu {script}\n",
        passed = shell_single_quote_value(PREBUILD_PASSED_MARKER),
        failed = shell_single_quote_value(PREBUILD_FAILED_MARKER),
        wrappers = shell_single_quote_value(&wrapper_dir),
        staging = shell_single_quote_value(STAGING_MOUNT),
        case_mount = shell_single_quote_value(CASE_MOUNT),
        work_mount = shell_single_quote_value(WORK_MOUNT),
        current_dir = shell_single_quote_value(&current_dir),
        script = shell_single_quote_value(&script),
        host_epoch = host_epoch,
    ))
}

fn run_prebuild_qemu(
    qemu: PathBuf,
    spec: crate::context::LinuxQemuSpec,
    kernel: &Path,
    rootfs: &Path,
    case: &TestQemuCase,
    layout: &case_assets::CaseAssetLayout,
    init_path: &Path,
) -> anyhow::Result<()> {
    let mut kernel_args = vec![
        format!("console={}", spec.console),
        "root=/dev/vda".to_string(),
        "rw".to_string(),
        "rootwait".to_string(),
        "init=/bin/sh".to_string(),
        "quiet".to_string(),
        "loglevel=3".to_string(),
    ];
    kernel_args.extend(spec.kernel_args.iter().map(|arg| (*arg).to_string()));

    let mut command = Command::new(&qemu);
    command
        .args(["-display", "none", "-monitor", "none", "-serial", "stdio"])
        .args(["-no-reboot", "-m", "512M", "-smp", "2"])
        .args(["-machine", spec.machine]);
    if let Some(cpu) = spec.cpu {
        command.args(["-cpu", cpu]);
    }
    command
        .arg("-kernel")
        .arg(kernel)
        .args(["-append", &kernel_args.join(" ")])
        .args([
            "-drive",
            &format!(
                "id=root,if=none,format=raw,snapshot=on,file={}",
                qemu_option_escape(rootfs)
            ),
        ])
        .args(["-device", "virtio-blk-pci,drive=root"])
        .args(["-netdev", "user,id=net0"])
        .args(["-device", &format!("{},netdev=net0", spec.network_device)]);
    add_virtfs(&mut command, "axbuild-staging", &layout.staging_root);
    add_virtfs(&mut command, "axbuild-case", &case.case_dir);
    add_virtfs(&mut command, "axbuild-work", &layout.work_dir);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let guest_init = guest_work_path(layout, init_path)?;
    let injection = format!(
        "mkdir -p {work} && mount -t 9p -o trans=virtio,version=9p2000.L,msize=1048576 \
         axbuild-work {work} && exec sh -eu {init}",
        work = shell_single_quote_value(WORK_MOUNT),
        init = shell_single_quote_value(&guest_init),
    );

    println!(
        "running prebuild.sh for `{}` with {} because qemu-user is unavailable",
        case.display_name,
        qemu.display()
    );
    monitor_prebuild_process(command, &qemu, Some(&injection), true)
}

fn add_virtfs(command: &mut Command, mount_tag: &str, path: &Path) {
    command.args([
        "-virtfs",
        &format!(
            "local,path={},mount_tag={mount_tag},security_model=none,multidevs=remap",
            qemu_option_escape(path)
        ),
    ]);
}

fn qemu_option_escape(path: &Path) -> String {
    path.display().to_string().replace(',', ",,")
}

fn guest_case_path(case: &TestQemuCase, path: &Path) -> anyhow::Result<String> {
    let relative = path.strip_prefix(&case.case_dir).with_context(|| {
        format!(
            "prebuild path {} is outside case directory {}",
            path.display(),
            case.case_dir.display()
        )
    })?;
    Ok(guest_path(CASE_MOUNT, relative))
}

fn guest_work_path(layout: &case_assets::CaseAssetLayout, path: &Path) -> anyhow::Result<String> {
    let relative = path.strip_prefix(&layout.work_dir).with_context(|| {
        format!(
            "prebuild path {} is outside work directory {}",
            path.display(),
            layout.work_dir.display()
        )
    })?;
    Ok(guest_path(WORK_MOUNT, relative))
}

fn guest_path(root: &str, relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        root.to_string()
    } else {
        format!("{root}/{}", relative.display())
    }
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn shell_single_quote_value(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_option_paths_escape_commas() {
        assert_eq!(qemu_option_escape(Path::new("/tmp/a,b")), "/tmp/a,,b");
    }
}
