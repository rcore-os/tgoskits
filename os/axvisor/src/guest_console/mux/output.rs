//! Line-safe arbitration for output from multiple guest consoles.

extern crate alloc;

use alloc::{collections::BTreeSet, format, vec::Vec};

/// Tracks the guest that owns the currently unterminated host-console line.
///
/// Ownership is preemptible at every backend-write boundary. If another guest
/// writes while the current line is unterminated, the formatter closes that
/// fragment and starts a newly prefixed line. A shell prompt therefore cannot
/// indefinitely block output from the foreground guest.
#[derive(Debug, Default)]
pub(crate) struct GuestOutputMux {
    host_line_owner: Option<usize>,
    host_line_prefixed: bool,
}

impl GuestOutputMux {
    /// Invalidates a continuation whose guest is no longer running.
    pub(crate) fn reconcile_running(&mut self, running: &BTreeSet<usize>) {
        if self
            .host_line_owner
            .is_some_and(|vm_id| !running.contains(&vm_id))
        {
            self.host_line_prefixed = false;
        }
    }

    /// Invalidates continuation state for a replaced or stopped backend.
    pub(crate) fn reset_guest(&mut self, vm_id: usize) {
        if self.host_line_owner == Some(vm_id) {
            self.host_line_prefixed = false;
        }
    }

    /// Formats one serialized backend write for the physical host console.
    pub(crate) fn format(&mut self, vm_id: usize, multiple_running: bool, bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(bytes.len() + 16);

        for &byte in bytes {
            if multiple_running {
                self.begin_prefixed_fragment(vm_id, &mut output);
            } else {
                self.begin_unprefixed_fragment(vm_id, &mut output);
            }

            output.push(byte);
            if byte == b'\n' {
                self.host_line_owner = None;
                self.host_line_prefixed = false;
            }
        }

        output
    }

    fn begin_prefixed_fragment(&mut self, vm_id: usize, output: &mut Vec<u8>) {
        if self.host_line_owner == Some(vm_id) && self.host_line_prefixed {
            return;
        }
        if self.host_line_owner.is_some() {
            output.push(b'\n');
        }
        output.extend_from_slice(format!("[VM {vm_id}] ").as_bytes());
        self.host_line_owner = Some(vm_id);
        self.host_line_prefixed = true;
    }

    fn begin_unprefixed_fragment(&mut self, vm_id: usize, output: &mut Vec<u8>) {
        if self.host_line_owner.is_some_and(|owner| owner != vm_id) {
            output.push(b'\n');
            self.host_line_owner = None;
        }
        if self.host_line_owner.is_none() {
            self.host_line_owner = Some(vm_id);
            self.host_line_prefixed = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unterminated_prompt_cannot_block_another_guest() {
        let mut mux = GuestOutputMux::default();

        assert_eq!(mux.format(2, true, b"~ # "), b"[VM 2] ~ # ");
        assert_eq!(
            mux.format(1, true, b"echo ok\nok\n"),
            b"\n[VM 1] echo ok\n[VM 1] ok\n"
        );
    }

    #[test]
    fn fragmented_output_from_one_guest_keeps_one_prefix() {
        let mut mux = GuestOutputMux::default();

        assert_eq!(mux.format(1, true, b"prom"), b"[VM 1] prom");
        assert_eq!(mux.format(1, true, b"pt\nnext\n"), b"pt\n[VM 1] next\n");
    }
}
