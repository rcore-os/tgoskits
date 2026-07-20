use super::*;

// QEMU platform, firmware, CPU, and acceleration flags come from the selected TOML file. The
// helpers retained here only apply explicit test controls such as `--smp`, snapshot persistence,
// and timeout scaling.
pub(super) struct QemuArgs<'a> {
    args: &'a [String],
}

impl<'a> QemuArgs<'a> {
    fn new(args: &'a [String]) -> Self {
        Self { args }
    }

    fn option_value(&self, option: &str) -> Option<&str> {
        let index = self.args.iter().position(|arg| arg == option)?;
        self.args.get(index + 1).map(String::as_str)
    }
}

pub(super) struct QemuArgsMut<'a> {
    args: &'a mut Vec<String>,
}

impl<'a> QemuArgsMut<'a> {
    fn new(args: &'a mut Vec<String>) -> Self {
        Self { args }
    }

    fn set_option_value(&mut self, option: &str, value: String) {
        if let Some(index) = self.args.iter().position(|arg| arg == option)
            && let Some(existing) = self.args.get_mut(index + 1)
        {
            *existing = value;
            return;
        }

        self.args.push(option.to_string());
        self.args.push(value);
    }
}

pub(crate) fn apply_smp_qemu_arg(qemu: &mut QemuConfig, smp: Option<usize>) {
    let Some(cpu_num) = smp else {
        return;
    };

    QemuArgsMut::new(&mut qemu.args).set_option_value("-smp", cpu_num.to_string());
}

pub(crate) fn apply_drive_snapshot_without_global_snapshot(qemu: &mut QemuConfig) {
    let mut global_snapshot = false;
    qemu.args.retain(|arg| {
        let keep = arg != "-snapshot";
        if !keep {
            global_snapshot = true;
        }
        keep
    });
    if !global_snapshot {
        return;
    }

    for index in 0..qemu.args.len() {
        if qemu.args.get(index).is_some_and(|arg| arg == "-drive")
            && let Some(drive) = qemu.args.get_mut(index + 1)
        {
            ensure_drive_snapshot_on(drive);
        }
    }
}

pub(super) fn ensure_drive_snapshot_on(drive: &mut String) {
    let mut replaced = false;
    let parts = drive
        .split(',')
        .map(|part| {
            if part.starts_with("snapshot=") {
                replaced = true;
                "snapshot=on".to_string()
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>();
    if replaced {
        *drive = parts.join(",");
    } else {
        drive.push_str(",snapshot=on");
    }
}

pub(crate) fn smp_from_qemu_arg(qemu: &QemuConfig) -> Option<usize> {
    let args = QemuArgs::new(&qemu.args);
    let value = args.option_value("-smp")?;
    parse_smp_qemu_value(value)
}

pub(super) fn parse_smp_qemu_value(value: &str) -> Option<usize> {
    let first = value.split(',').next()?;
    if let Ok(cpu_num) = first.parse() {
        return Some(cpu_num);
    }

    value.split(',').find_map(|part| {
        let cpu_num = part.strip_prefix("cpus=")?;
        cpu_num.parse().ok()
    })
}

pub(crate) fn apply_timeout_scale(qemu: &mut QemuConfig) {
    let Some(timeout) = qemu.timeout else {
        return;
    };
    if timeout == 0 {
        return;
    }

    let scale = match std::env::var(TIMEOUT_SCALE_ENV) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(scale) if scale > 1 => scale,
            Ok(_) | Err(_) => {
                eprintln!(
                    "warning: ignoring invalid {TIMEOUT_SCALE_ENV} value `{}`; expected integer > \
                     1",
                    value.trim()
                );
                return;
            }
        },
        Err(_) => return,
    };

    qemu.timeout = timeout.checked_mul(scale).or(Some(u64::MAX));
}

pub(crate) fn qemu_timeout_summary(qemu: &QemuConfig) -> String {
    match qemu.timeout {
        Some(0) | None => "disabled".to_string(),
        Some(timeout) => format!("{timeout}s"),
    }
}
