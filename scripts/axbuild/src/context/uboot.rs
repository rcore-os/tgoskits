use ostool::run::uboot::UbootConfig;

/// Ostool's `LocalBackend` reads reset/power-off commands from `UbootConfig.local`.
/// Top-level TOML fields still deserialize onto the outer config, so copy them into
/// `local` when the backend-specific section is empty.
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
    fn normalize_moves_top_level_reset_cmds_into_local() {
        let mut config: UbootConfig = toml::from_str(
            r#"
serial = "/dev/ttyUSB0"
baud_rate = "1500000"
success_regex = []
fail_regex = []
board_reset_cmd = "reset"
board_power_off_cmd = "poweroff"
[net]
interface = "enp3s0"
tftp_dir = "/tmp/ostool-tftp"
"#,
        )
        .unwrap();

        assert!(config.local.net.is_some());
        assert!(config.local.board_reset_cmd.is_none());
        assert_eq!(config.board_reset_cmd.as_deref(), Some("reset"));

        normalize_uboot_config_for_local_backend(&mut config);

        assert!(config.board_reset_cmd.is_none());
        assert_eq!(config.local.board_reset_cmd.as_deref(), Some("reset"));
        assert_eq!(
            config.local.board_power_off_cmd.as_deref(),
            Some("poweroff")
        );
        let net = config.local.net.as_ref().unwrap();
        assert_eq!(net.interface, "enp3s0");
        assert_eq!(net.tftp_dir.as_deref(), Some("/tmp/ostool-tftp"));
    }
}
