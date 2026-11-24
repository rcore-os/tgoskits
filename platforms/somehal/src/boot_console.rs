use rdrive::{DeviceId, Fdt, probe::acpi::AcpiResourceAddress};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsoleSpec {
    HardwareSerial(usize),
    RockchipFiq(usize),
    AcpiSerial(Option<AcpiResourceAddress>),
    VirtualTty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleDeviceIdError {
    NotSpecified,
    NoHardwareDevice,
    DeviceNotFound,
}

pub fn device_id() -> Result<DeviceId, ConsoleDeviceIdError> {
    match device_id_from_bootargs(someboot::cmdline()) {
        Ok(device_id) => Ok(device_id),
        Err(ConsoleDeviceIdError::NotSpecified) => device_id_from_acpi_spcr()
            .or_else(device_id_from_fdt_stdout)
            .ok_or(ConsoleDeviceIdError::NotSpecified),
        Err(
            err @ (ConsoleDeviceIdError::NoHardwareDevice | ConsoleDeviceIdError::DeviceNotFound),
        ) => Err(err),
    }
}

fn device_id_from_bootargs(cmdline: Option<&str>) -> Result<DeviceId, ConsoleDeviceIdError> {
    device_id_from_bootargs_with(
        cmdline,
        device_id_from_serial_index,
        device_id_from_rockchip_fiq_index,
        rdrive::acpi_resource_address_to_device_id,
    )
}

fn device_id_from_bootargs_with(
    cmdline: Option<&str>,
    serial_device_id: impl Fn(usize) -> Option<DeviceId>,
    rockchip_fiq_device_id: impl Fn(usize) -> Option<DeviceId>,
    acpi_address_device_id: impl Fn(AcpiResourceAddress) -> Option<DeviceId>,
) -> Result<DeviceId, ConsoleDeviceIdError> {
    let cmdline = cmdline.ok_or(ConsoleDeviceIdError::NotSpecified)?;
    let mut has_console_spec = false;
    let mut last_hardware_device = None;

    for spec in console_specs(cmdline) {
        has_console_spec = true;
        match spec {
            ConsoleSpec::HardwareSerial(index) => {
                last_hardware_device = Some(serial_device_id(index));
            }
            ConsoleSpec::RockchipFiq(index) => {
                last_hardware_device = Some(rockchip_fiq_device_id(index));
            }
            ConsoleSpec::AcpiSerial(address) => {
                last_hardware_device = Some(address.and_then(&acpi_address_device_id));
            }
            ConsoleSpec::VirtualTty => {}
        }
    }

    match last_hardware_device {
        Some(Some(device_id)) => Ok(device_id),
        Some(None) => Err(ConsoleDeviceIdError::DeviceNotFound),
        None if has_console_spec => Err(ConsoleDeviceIdError::NoHardwareDevice),
        None => Err(ConsoleDeviceIdError::NotSpecified),
    }
}

fn console_specs(cmdline: &str) -> impl Iterator<Item = ConsoleSpec> + '_ {
    cmdline
        .split_ascii_whitespace()
        .filter_map(|arg| arg.strip_prefix("console="))
        .filter_map(parse_console_spec)
}

fn parse_console_spec(spec: &str) -> Option<ConsoleSpec> {
    let name = spec.split(',').next().unwrap_or(spec);
    if name == "uart" || name == "uart8250" {
        return Some(ConsoleSpec::AcpiSerial(parse_acpi_serial_address(spec)));
    }

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
        .or_else(|| parse_number_suffix(name, "ttyFIQ").map(ConsoleSpec::RockchipFiq))
}

fn parse_acpi_serial_address(spec: &str) -> Option<AcpiResourceAddress> {
    let mut fields = spec.split(',');
    let name = fields.next()?;
    if name != "uart" && name != "uart8250" {
        return None;
    }

    let space = fields.next()?;
    let address = fields.next()?;
    if !address.starts_with("0x") || address.len() <= 2 {
        return None;
    }
    let digits = &address[2..];
    if !digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
        return None;
    }
    let base = u64::from_str_radix(digits, 16).ok()?;
    if base == 0 || usize::try_from(base).is_err() {
        return None;
    }

    match space {
        "io" => Some(AcpiResourceAddress::io(base)),
        "mmio" => Some(AcpiResourceAddress::memory(base)),
        _ => None,
    }
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

fn device_id_from_rockchip_fiq_index(index: usize) -> Option<DeviceId> {
    rdrive::with_fdt(|fdt| {
        fdt.find_compatible(&["rockchip,fiq-debugger"])
            .into_iter()
            .filter_map(|node| rdrive::fdt_path_to_device_id(&node.path()))
            .nth(index)
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
    fn console_specs_keep_command_line_order() {
        let specs: alloc::vec::Vec<_> =
            console_specs("console=ttyS2,1500000 console=tty1 console=ttyAMA3,115200").collect();

        assert_eq!(
            specs,
            alloc::vec![
                ConsoleSpec::HardwareSerial(2),
                ConsoleSpec::VirtualTty,
                ConsoleSpec::HardwareSerial(3),
            ]
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
    fn parses_rockchip_fiq_console_name() {
        assert_eq!(
            parse_console_spec("ttyFIQ0,1500000"),
            Some(ConsoleSpec::RockchipFiq(0))
        );
    }

    #[test]
    fn bootargs_console_spec_suppresses_firmware_fallback() {
        assert_eq!(
            device_id_from_bootargs(Some("root=/dev/vda")),
            Err(ConsoleDeviceIdError::NotSpecified)
        );
        assert_eq!(
            device_id_from_bootargs(Some("console=tty1")),
            Err(ConsoleDeviceIdError::NoHardwareDevice)
        );
        assert_eq!(
            device_id_from_bootargs(Some("console=ttyS2 console=tty1")),
            Err(ConsoleDeviceIdError::DeviceNotFound)
        );
    }

    #[test]
    fn later_virtual_console_does_not_hide_hardware_device_id() {
        let serial2_device = DeviceId::from(42);

        assert_eq!(
            device_id_from_bootargs_with(
                Some("console=ttyS2,1500000 console=tty1"),
                |index| (index == 2).then_some(serial2_device),
                |_| None,
                |_| None,
            ),
            Ok(serial2_device)
        );
    }

    #[test]
    fn last_available_hardware_console_wins_over_earlier_serial_console() {
        let serial3_device = DeviceId::from(43);

        assert_eq!(
            device_id_from_bootargs_with(
                Some("console=ttyS2,1500000 console=tty1 console=ttyAMA3,115200"),
                |index| (index == 3).then_some(serial3_device),
                |_| None,
                |_| None,
            ),
            Ok(serial3_device)
        );
    }

    #[test]
    fn later_missing_hardware_console_overrides_earlier_available_serial_console() {
        let serial2_device = DeviceId::from(42);

        assert_eq!(
            device_id_from_bootargs_with(
                Some("console=ttyS2,1500000 console=ttyS3,115200 console=tty1"),
                |index| (index == 2).then_some(serial2_device),
                |_| None,
                |_| None,
            ),
            Err(ConsoleDeviceIdError::DeviceNotFound)
        );
        assert_eq!(
            device_id_from_bootargs_with(
                Some("console=ttyS2,1500000 console=ttyS3,115200"),
                |index| (index == 2).then_some(serial2_device),
                |_| None,
                |_| None,
            ),
            Err(ConsoleDeviceIdError::DeviceNotFound)
        );
    }

    #[test]
    fn rockchip_fiq_console_uses_fiq_device_resolver() {
        let fiq_device = DeviceId::from(44);

        assert_eq!(
            device_id_from_bootargs_with(
                Some("console=ttyFIQ0,1500000"),
                |_| None,
                |index| (index == 0).then_some(fiq_device),
                |_| None,
            ),
            Ok(fiq_device)
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
    fn explicit_acpi_address_selects_registered_device_and_fails_without_fallback() {
        let serial_device = DeviceId::from(42);
        let acpi_device = DeviceId::from(45);
        let first_address = AcpiResourceAddress::memory(0x1fe0_01e0);
        let address = AcpiResourceAddress::memory(0x1fe0_01e8);

        assert_eq!(
            device_id_from_bootargs_with(
                Some(
                    "console=ttyS2 console=uart,mmio,0x1fe001e0 \
                     console=uart8250,mmio,0x1fe001e8,115200n8 console=tty1"
                ),
                |index| (index == 2).then_some(serial_device),
                |_| None,
                |selected| {
                    if selected == address {
                        Some(acpi_device)
                    } else if selected == first_address {
                        Some(serial_device)
                    } else {
                        None
                    }
                },
            ),
            Ok(acpi_device)
        );
        assert_eq!(
            device_id_from_bootargs_with(
                Some("console=ttyS2 console=uart,mmio,0x1fe001e0"),
                |index| (index == 2).then_some(serial_device),
                |_| None,
                |_| None,
            ),
            Err(ConsoleDeviceIdError::DeviceNotFound)
        );
        assert_eq!(
            device_id_from_bootargs_with(
                Some("console=ttyS2 console=uart8250,mmio,0x0"),
                |index| (index == 2).then_some(serial_device),
                |_| None,
                |_| Some(acpi_device),
            ),
            Err(ConsoleDeviceIdError::DeviceNotFound)
        );
        assert_eq!(
            device_id_from_bootargs_with(
                Some("console=ttyS2 console=uart,mmio32,0x1fe001e0"),
                |index| (index == 2).then_some(serial_device),
                |_| None,
                |_| Some(acpi_device),
            ),
            Err(ConsoleDeviceIdError::DeviceNotFound)
        );
    }

    #[test]
    fn parses_acpi_address_spaces_and_rejects_unsupported_forms() {
        assert_eq!(
            parse_console_spec("uart,io,0x3f8"),
            Some(ConsoleSpec::AcpiSerial(Some(AcpiResourceAddress::io(
                0x3f8
            ))))
        );
        assert_eq!(
            parse_console_spec("uart8250,mmio,0x1fe001e0,115200n8"),
            Some(ConsoleSpec::AcpiSerial(Some(AcpiResourceAddress::memory(
                0x1fe0_01e0
            ))))
        );
        for spec in [
            "uart,mmio32,0x1000",
            "uart,mmio,1000",
            "uart,mmio,0x",
            "uart,mmio,0x+1",
            "uart,mmio,0x10000000000000000",
        ] {
            assert_eq!(
                parse_console_spec(spec),
                Some(ConsoleSpec::AcpiSerial(None)),
                "{spec}"
            );
        }
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
