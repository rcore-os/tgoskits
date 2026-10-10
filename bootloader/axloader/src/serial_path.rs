use alloc::vec::Vec;

// UEFI 2.10, device path type/subtype assignments used for UART matching.
const DEVICE_PATH_END_TYPE: u8 = 0x7f;
const MESSAGING_DEVICE_PATH_TYPE: u8 = 0x03;
const UART_DEVICE_PATH_SUBTYPE: u8 = 0x0e;
const VENDOR_DEVICE_PATH_SUBTYPE: u8 = 0x0a;

/// Match complete device-path nodes; UART attributes may use firmware defaults.
pub fn console_matches(console: &[u8], serial: &[u8]) -> bool {
    let Some(serial_nodes) = nodes(serial) else {
        return false;
    };
    let Some(console_nodes) = nodes(console) else {
        return false;
    };
    let prefix = serial_nodes
        .into_iter()
        .take_while(|n| n[0] != DEVICE_PATH_END_TYPE)
        .collect::<Vec<_>>();
    if prefix.is_empty() {
        return false;
    }
    let mut start = 0;
    for (end, node) in console_nodes.iter().enumerate() {
        if node[0] != DEVICE_PATH_END_TYPE {
            continue;
        }
        let instance = &console_nodes[start..end];
        if instance.len() >= prefix.len()
            && prefix.iter().zip(instance).all(|(a, b)| {
                if a.len() != b.len() {
                    false
                } else if a[0] == MESSAGING_DEVICE_PATH_TYPE && a[1] == UART_DEVICE_PATH_SUBTYPE {
                    b[0] == MESSAGING_DEVICE_PATH_TYPE && b[1] == UART_DEVICE_PATH_SUBTYPE
                } else {
                    a == b
                }
            })
            && instance[prefix.len()..]
                .iter()
                .all(|n| n[0] == MESSAGING_DEVICE_PATH_TYPE && n[1] == VENDOR_DEVICE_PATH_SUBTYPE)
        {
            return true;
        }
        start = end + 1;
    }
    false
}

fn nodes(mut bytes: &[u8]) -> Option<Vec<&[u8]>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let head = bytes.get(..4)?;
        let size = u16::from_le_bytes([head[2], head[3]]) as usize;
        if size < 4 {
            return None;
        }
        result.push(bytes.get(..size)?);
        bytes = bytes.get(size..)?;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    fn node(node_type: u8, subtype: u8, payload: &[u8]) -> Vec<u8> {
        let size = 4 + payload.len();
        let mut bytes = vec![node_type, subtype, size as u8, (size >> 8) as u8];
        bytes.extend_from_slice(payload);
        bytes
    }

    fn end() -> Vec<u8> {
        node(0x7f, 0xff, &[])
    }

    fn path(nodes: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = nodes.iter().flatten().copied().collect::<Vec<_>>();
        bytes.extend(end());
        bytes
    }

    #[test]
    fn nodes_reject_short_and_truncated_nodes() {
        assert!(nodes(&[3, 14, 3, 0]).is_none());
        assert!(nodes(&[3, 14, 8, 0, 1]).is_none());
    }

    #[test]
    fn console_matching_ignores_uart_attributes() {
        let hardware = node(1, 1, &[0xaa, 0xbb]);
        let serial = node(3, 14, &[1, 2, 3, 4]);
        let firmware_serial = node(3, 14, &[9, 8, 7, 6]);
        assert!(console_matches(
            &path(&[hardware.clone(), firmware_serial]),
            &path(&[hardware, serial,])
        ));
    }

    #[test]
    fn console_matching_checks_each_device_path_instance() {
        let hardware = node(1, 1, &[0xaa, 0xbb]);
        let serial = node(3, 14, &[1, 2, 3, 4]);
        let other = node(1, 1, &[0xcc, 0xdd]);
        let console = [
            path(&[other, node(3, 14, &[9, 8, 7, 6])]),
            path(&[hardware.clone(), serial.clone(), node(3, 10, &[])]),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        assert!(console_matches(&console, &path(&[hardware, serial])));
    }

    #[test]
    fn console_matching_requires_a_complete_serial_prefix() {
        let hardware = node(1, 1, &[0xaa, 0xbb]);
        let serial = node(3, 14, &[1, 2, 3, 4]);
        let mismatched = node(1, 2, &[0xaa, 0xbb]);
        assert!(!console_matches(
            &path(&[mismatched, serial.clone()]),
            &path(&[hardware, serial]),
        ));
    }
}
