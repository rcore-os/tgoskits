use rdrive::{DeviceId, Fdt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsoleSpec {
    HardwareSerial(usize),
    VirtualTty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BootargsConsoleError {
    NotSpecified,
    NoHardwareDevice,
}

pub fn device_id() -> Option<DeviceId> {
    match device_id_from_bootargs(someboot::cmdline()) {
        Ok(device_id) => Some(device_id),
        Err(BootargsConsoleError::NotSpecified) => {
            device_id_from_acpi_spcr().or_else(device_id_from_fdt_stdout)
        }
        Err(BootargsConsoleError::NoHardwareDevice) => None,
    }
}

fn device_id_from_bootargs(cmdline: Option<&str>) -> Result<DeviceId, BootargsConsoleError> {
    let spec = last_console_spec(cmdline.ok_or(BootargsConsoleError::NotSpecified)?)
        .ok_or(BootargsConsoleError::NotSpecified)?;

    match spec {
        ConsoleSpec::HardwareSerial(index) => {
            device_id_from_serial_index(index).ok_or(BootargsConsoleError::NoHardwareDevice)
        }
        ConsoleSpec::VirtualTty => Err(BootargsConsoleError::NoHardwareDevice),
    }
}

fn last_console_spec(cmdline: &str) -> Option<ConsoleSpec> {
    cmdline
        .split_ascii_whitespace()
        .filter_map(|arg| arg.strip_prefix("console="))
        .filter_map(parse_console_spec)
        .next_back()
}

fn parse_console_spec(spec: &str) -> Option<ConsoleSpec> {
    let name = spec.split(',').next().unwrap_or(spec);
    if name == "tty"
        || name == "ttynull"
        || name
            .strip_prefix("tty")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_digit()))
    {
        return Some(ConsoleSpec::VirtualTty);
    }

    parse_number_suffix(name, "ttyS")
        .or_else(|| parse_number_suffix(name, "ttyAMA"))
        .map(ConsoleSpec::HardwareSerial)
}

fn parse_number_suffix(name: &str, prefix: &str) -> Option<usize> {
    name.strip_prefix(prefix)?.parse().ok()
}

fn device_id_from_serial_index(index: usize) -> Option<DeviceId> {
    device_id_from_serial_index_with(index, fdt_serial_alias_device_id, device_id_from_acpi_spcr)
}

fn device_id_from_serial_index_with(
    index: usize,
    fdt_device_id: impl FnOnce(usize) -> Option<DeviceId>,
    spcr_device_id: impl FnOnce() -> Option<DeviceId>,
) -> Option<DeviceId> {
    fdt_device_id(index).or_else(|| if index == 0 { spcr_device_id() } else { None })
}

fn fdt_serial_alias_device_id(index: usize) -> Option<DeviceId> {
    rdrive::with_fdt(|fdt| {
        let alias = alloc::format!("serial{index}");
        let path = alias_path(fdt, &alias)?;
        rdrive::fdt_path_to_device_id(path)
    })
    .flatten()
}

fn device_id_from_acpi_spcr() -> Option<DeviceId> {
    rdrive::acpi_spcr_console_device_id()
}

fn device_id_from_fdt_stdout() -> Option<DeviceId> {
    rdrive::with_fdt(stdout_device_id).flatten()
}

fn stdout_device_id(fdt: &Fdt) -> Option<DeviceId> {
    let chosen = fdt.get_by_path("/chosen")?;
    ["stdout-path", "linux,stdout-path"]
        .into_iter()
        .find_map(|key| {
            let raw = chosen.as_node().get_property(key)?.as_str()?;
            let path = split_stdout_options(raw);
            if path.is_empty() {
                return None;
            }
            if path.starts_with('/') {
                return rdrive::fdt_path_to_device_id(path);
            }
            alias_path(fdt, path).and_then(rdrive::fdt_path_to_device_id)
        })
}

fn split_stdout_options(stdout: &str) -> &str {
    stdout.split(':').next().unwrap_or(stdout)
}

fn alias_path<'a>(fdt: &'a Fdt, alias: &str) -> Option<&'a str> {
    fdt.get_by_path("/aliases")?
        .as_node()
        .get_property(alias)?
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_console_spec_wins() {
        assert_eq!(
            last_console_spec("console=ttyS2,1500000 console=tty1"),
            Some(ConsoleSpec::VirtualTty)
        );
        assert_eq!(
            last_console_spec("console=tty1 console=ttyAMA3,115200"),
            Some(ConsoleSpec::HardwareSerial(3))
        );
    }

    #[test]
    fn parses_supported_serial_console_names() {
        assert_eq!(
            parse_console_spec("ttyS0,115200n8"),
            Some(ConsoleSpec::HardwareSerial(0))
        );
        assert_eq!(
            parse_console_spec("ttyAMA1"),
            Some(ConsoleSpec::HardwareSerial(1))
        );
        assert_eq!(parse_console_spec("ttynull"), Some(ConsoleSpec::VirtualTty));
        assert_eq!(parse_console_spec("tty7"), Some(ConsoleSpec::VirtualTty));
        assert_eq!(parse_console_spec("ttySx"), None);
    }

    #[test]
    fn bootargs_console_spec_suppresses_firmware_fallback() {
        assert_eq!(
            device_id_from_bootargs(Some("root=/dev/vda")),
            Err(BootargsConsoleError::NotSpecified)
        );
        assert_eq!(
            device_id_from_bootargs(Some("console=tty1")),
            Err(BootargsConsoleError::NoHardwareDevice)
        );
        assert_eq!(
            device_id_from_bootargs(Some("console=ttyS2 console=tty1")),
            Err(BootargsConsoleError::NoHardwareDevice)
        );
    }

    #[test]
    fn serial_index_zero_can_use_acpi_spcr_when_fdt_alias_is_absent() {
        let spcr_device = DeviceId::from(42);

        assert_eq!(
            device_id_from_serial_index_with(0, |_| None, || Some(spcr_device)),
            Some(spcr_device)
        );
    }

    #[test]
    fn non_zero_serial_index_does_not_fallback_to_acpi_spcr() {
        let spcr_device = DeviceId::from(42);

        assert_eq!(
            device_id_from_serial_index_with(2, |_| None, || Some(spcr_device)),
            None
        );
    }

    #[test]
    fn splits_stdout_options() {
        assert_eq!(
            split_stdout_options("/soc/serial@1000:115200n8"),
            "/soc/serial@1000"
        );
        assert_eq!(split_stdout_options("serial0"), "serial0");
    }
}
