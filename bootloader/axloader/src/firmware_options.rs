//! Explicit options supplied by the operator to a native EFI application.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_options_require_explicit_valid_console_and_boot_nonce() {
        let defaults = FirmwareOptions::parse("").unwrap();
        assert_eq!(defaults.console(), None);
        assert_eq!(defaults.boot_nonce(), None);
        let valid = r#"{"console":{"baud_rate":115200,"data_bits":8,"parity":"none","stop_bits":"one","flow_control":"none"},"boot_nonce":"0123456789abcdef0123456789abcdef"}"#;
        let configured = FirmwareOptions::parse(valid).unwrap();
        assert_eq!(configured.console().unwrap().baud_rate, 115_200);
        assert_eq!(configured.boot_nonce().unwrap()[..4], [1, 35, 69, 103]);
        for invalid in [
            valid.replace("115200", "0"),
            valid.replace("\"data_bits\":8", "\"data_bits\":6"),
            valid.replace("0123456789abcdef0123456789abcdef", "short"),
            valid.replace("0123456789abcdef0123456789abcdef", "z".repeat(32).as_str()),
            r#"{"boot_nonce":"0123456789abcdef0123456789abcdef","boot_nonce":"fedcba9876543210fedcba9876543210"}"#.into(),
            r#"{"guess_console":true}"#.into(),
        ] {
            assert!(FirmwareOptions::parse(&invalid).is_err(), "{invalid}");
        }
    }
}
