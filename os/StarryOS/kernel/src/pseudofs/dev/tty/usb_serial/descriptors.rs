#[derive(Clone, Copy)]
pub(super) struct InterfaceDescriptor {
    pub(super) number: u8,
    pub(super) alternate_setting: u8,
    pub(super) class: u8,
    pub(super) subclass: u8,
    pub(super) protocol: u8,
}

pub(super) fn bulk_pair_for_interface(
    blob: &[u8],
    mut accept_interface: impl FnMut(InterfaceDescriptor) -> bool,
) -> Option<(u8, u8, u8)> {
    // The usbfs snapshot stores the device descriptor first. Skip it and walk
    // the raw configuration descriptor tree, because Starry does not yet keep a
    // parsed per-interface USB descriptor model for tty backends to reuse.
    let mut cursor = 18;
    let mut current_interface = None;
    let mut current_in = None;
    let mut current_out = None;
    let mut current_accepted = false;
    while cursor + 2 <= blob.len() {
        let len = blob[cursor] as usize;
        let ty = blob[cursor + 1];
        if len < 2 || cursor + len > blob.len() {
            break;
        }

        match ty {
            0x04 if len >= 9 => {
                if current_accepted
                    && let Some(result) =
                        finish_bulk_pair(current_interface, current_in, current_out)
                {
                    return Some(result);
                }
                let interface = InterfaceDescriptor {
                    number: blob[cursor + 2],
                    alternate_setting: blob[cursor + 3],
                    class: blob[cursor + 5],
                    subclass: blob[cursor + 6],
                    protocol: blob[cursor + 7],
                };
                current_interface = Some(interface.number);
                current_in = None;
                current_out = None;
                current_accepted = accept_interface(interface);
            }
            0x05 if current_accepted && len >= 7 => {
                let address = blob[cursor + 2];
                let attributes = blob[cursor + 3] & 0x03;
                if attributes == 0x02 {
                    if address & 0x80 != 0 {
                        current_in = Some(address);
                    } else {
                        current_out = Some(address);
                    }
                }
            }
            _ => {}
        }
        cursor += len;
    }

    current_accepted
        .then_some(())
        .and_then(|()| finish_bulk_pair(current_interface, current_in, current_out))
}

fn finish_bulk_pair(
    interface: Option<u8>,
    bulk_in: Option<u8>,
    bulk_out: Option<u8>,
) -> Option<(u8, u8, u8)> {
    Some((interface?, bulk_in?, bulk_out?))
}
