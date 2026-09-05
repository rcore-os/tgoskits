/// Reads the next control byte from the firmware-staged input or raw serial input.
///
/// UEFI's terminal driver can move bytes from the serial device into its own input
/// queue asynchronously. Bytes already staged by the firmware therefore precede
/// bytes that remain available from the raw serial device.
pub(crate) fn next_serial_control_byte(
    firmware_input_available: bool,
    mut firmware_input: impl FnMut() -> Option<u8>,
    mut raw_serial_input: impl FnMut() -> Option<u8>,
) -> Option<u8> {
    if firmware_input_available {
        firmware_input().or_else(&mut raw_serial_input)
    } else {
        raw_serial_input()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::next_serial_control_byte;

    #[test]
    fn drains_firmware_staged_bytes_before_raw_serial_bytes() {
        let mut firmware_input = VecDeque::from(*b"older");
        let mut raw_serial_input = VecDeque::from(*b"newer");
        let mut received = Vec::new();

        while !firmware_input.is_empty() || !raw_serial_input.is_empty() {
            received.push(
                next_serial_control_byte(
                    true,
                    || firmware_input.pop_front(),
                    || raw_serial_input.pop_front(),
                )
                .unwrap(),
            );
        }

        assert_eq!(received, b"oldernewer");
    }

    #[test]
    fn falls_back_to_raw_serial_when_firmware_has_no_byte() {
        assert_eq!(
            next_serial_control_byte(true, || None, || Some(b'x')),
            Some(b'x')
        );
    }

    #[test]
    fn skips_firmware_reader_when_firmware_input_is_unavailable() {
        assert_eq!(
            next_serial_control_byte(
                false,
                || panic!("unavailable firmware input must not be read"),
                || Some(b'x'),
            ),
            Some(b'x')
        );
    }
}
