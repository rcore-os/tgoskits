use ostool::run::uboot::UbootConfig;

/// Move legacy top-level board-control fields to the local backend configuration.
pub(crate) fn normalize_uboot_config_for_local_backend(config: &mut UbootConfig) {
    if config.local.board_reset_cmd.is_none() {
        config.local.board_reset_cmd = config.board_reset_cmd.take();
    }
    if config.local.board_power_off_cmd.is_none() {
        config.local.board_power_off_cmd = config.board_power_off_cmd.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_net_into_local_backend_config() {
        let mut config: UbootConfig = toml::from_str(
            r#"
serial = "/dev/ttyUSB0"
baud_rate = "1500000"
success_regex = []
fail_regex = []
[net]
interface = "enp3s0"
tftp_dir = "/tmp/ostool-tftp"
"#,
        )
        .unwrap();

        normalize_uboot_config_for_local_backend(&mut config);

        let net = config.local.net.as_ref().unwrap();
        assert_eq!(net.interface, "enp3s0");
        assert_eq!(net.tftp_dir.as_deref(), Some("/tmp/ostool-tftp"));
    }
}
