//! Line-safe arbitration for output from multiple guest consoles.

extern crate alloc;

use alloc::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    format,
    vec::Vec,
};

pub(super) const PER_GUEST_LOG_CAPACITY: usize = 16 * 1024;

/// Arbitrates complete host-console lines across guest serial backends.
#[derive(Debug, Default)]
pub(crate) struct GuestOutputMux {
    guests: BTreeMap<usize, GuestOutputState>,
    mode: OutputMode,
    owner: Option<usize>,
    physical_line_open: bool,
    preemption: Option<usize>,
    total_pending: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum OutputMode {
    #[default]
    BootMultiplex,
    Interactive {
        foreground: Option<usize>,
    },
}

#[derive(Debug)]
struct GuestOutputState {
    pending: VecDeque<u8>,
    at_line_start: bool,
}

#[cfg(test)]
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ArbitrationSnapshot {
    mode: OutputMode,
    owner: Option<usize>,
    physical_line_open: bool,
    preemption: Option<usize>,
    total_pending: usize,
    guests: Vec<(usize, usize, bool)>,
}

impl Default for GuestOutputState {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            at_line_start: true,
        }
    }
}

impl GuestOutputMux {
    /// Starts the boot-time mode that displays complete lines from every VM.
    pub(crate) fn start_boot_multiplex(&mut self) {
        self.mode = OutputMode::BootMultiplex;
    }

    /// Gives one interactive guest direct access to the host console.
    pub(crate) fn enter_interactive(&mut self, vm_id: usize) {
        self.mode = OutputMode::Interactive {
            foreground: Some(vm_id),
        };
    }

    /// Keeps every guest's output in its ring and terminates its open physical line.
    pub(crate) fn buffer_all(&mut self) -> Vec<u8> {
        let separator = if self.physical_line_open {
            Vec::from(*b"\n")
        } else {
            Vec::new()
        };
        self.mode = OutputMode::Interactive { foreground: None };
        self.preemption = None;
        if let Some(owner) = self.owner
            && let Some(guest) = self.guests.get_mut(&owner)
        {
            guest.at_line_start = true;
        }
        self.owner = None;
        self.physical_line_open = false;
        separator
    }

    /// Selects an interactive guest and returns its buffered console log.
    pub(crate) fn select_foreground(&mut self, vm_id: usize) -> Vec<u8> {
        self.enter_interactive(vm_id);
        self.drain_guest_log(vm_id)
    }

    /// Enters interactive mode on the first input and returns the buffered prompt.
    pub(crate) fn select_foreground_on_input(&mut self, vm_id: usize) -> Vec<u8> {
        match self.mode {
            OutputMode::Interactive {
                foreground: Some(foreground),
            } if foreground == vm_id => Vec::new(),
            _ => self.select_foreground(vm_id),
        }
    }

    /// Discards output state for guests that are no longer running.
    pub(crate) fn reconcile_running(&mut self, running: &BTreeSet<usize>) {
        let discarded = self
            .guests
            .iter()
            .filter(|(vm_id, _)| !running.contains(vm_id))
            .map(|(_, guest)| guest.pending.len())
            .sum::<usize>();
        self.guests.retain(|vm_id, _| running.contains(vm_id));
        self.total_pending -= discarded;
        if self.owner.is_some_and(|vm_id| !running.contains(&vm_id)) {
            self.owner = None;
        }
        if self
            .preemption
            .is_some_and(|vm_id| !running.contains(&vm_id))
        {
            self.preemption = None;
        }
    }

    /// Discards pending output for a replaced or stopped backend.
    pub(crate) fn reset_guest(&mut self, vm_id: usize) {
        if let Some(guest) = self.guests.remove(&vm_id) {
            self.total_pending -= guest.pending.len();
        }
        if self.owner == Some(vm_id) {
            self.owner = None;
        }
        if self.preemption == Some(vm_id) {
            self.preemption = None;
        }
    }

    /// Requests that the next write from `vm_id` take physical-line ownership.
    pub(crate) fn request_preemption(&mut self, vm_id: usize) {
        self.preemption = Some(vm_id);
    }

    /// Enqueues one backend write and returns bytes ready for the host console.
    pub(crate) fn format(&mut self, vm_id: usize, multiple_running: bool, bytes: &[u8]) -> Vec<u8> {
        if let OutputMode::Interactive { foreground } = self.mode {
            return self.format_interactive(vm_id, foreground, bytes);
        }

        self.append_log(vm_id, bytes);

        if !multiple_running {
            let pending = self
                .guests
                .get(&vm_id)
                .expect("guest output queue was just created")
                .pending
                .len();
            let mut output = Vec::with_capacity(pending.saturating_add(1));
            if self.physical_line_open && self.owner != Some(vm_id) {
                output.push(b'\n');
                self.physical_line_open = false;
            }
            self.owner = Some(vm_id);
            let guest = self
                .guests
                .get_mut(&vm_id)
                .expect("guest output queue was just created");
            self.total_pending -= guest.pending.len();
            output.extend(guest.pending.drain(..));
            for &byte in &output {
                guest.at_line_start = byte == b'\n';
                self.physical_line_open = byte != b'\n';
            }
            if !self.physical_line_open {
                self.owner = None;
            }
            return output;
        }

        let mut output = Vec::with_capacity(self.total_pending.saturating_add(16));
        loop {
            let preferred = self.preemption.filter(|vm_id| {
                self.guests
                    .get(vm_id)
                    .is_some_and(|guest| guest.pending.contains(&b'\n'))
            });
            let Some(next) = preferred.or_else(|| {
                self.guests
                    .iter()
                    .find_map(|(&vm_id, guest)| guest.pending.contains(&b'\n').then_some(vm_id))
            }) else {
                break;
            };
            if preferred == Some(next) {
                self.preemption = None;
            }

            if self.physical_line_open {
                output.push(b'\n');
                self.physical_line_open = false;
            }
            if let Some(previous_owner) = self.owner
                && let Some(previous_guest) = self.guests.get_mut(&previous_owner)
            {
                previous_guest.at_line_start = true;
            }
            self.owner = Some(next);
            output.extend_from_slice(format!("[VM {next}] ").as_bytes());

            let guest = self
                .guests
                .get_mut(&next)
                .expect("completed line must have guest state");
            guest.at_line_start = false;
            while let Some(byte) = guest.pending.pop_front() {
                self.total_pending -= 1;
                output.push(byte);
                self.physical_line_open = byte != b'\n';
                if byte == b'\n' {
                    guest.at_line_start = true;
                    self.owner = None;
                    break;
                }
            }
        }

        output
    }

    fn format_interactive(
        &mut self,
        vm_id: usize,
        foreground: Option<usize>,
        bytes: &[u8],
    ) -> Vec<u8> {
        if foreground != Some(vm_id) {
            self.append_log(vm_id, bytes);
            return Vec::new();
        }

        let mut output = self.drain_guest_log(vm_id);
        output.reserve(bytes.len());
        output.extend_from_slice(bytes);
        self.update_physical_line(vm_id, bytes);
        output
    }

    fn drain_guest_log(&mut self, vm_id: usize) -> Vec<u8> {
        let pending = self
            .guests
            .get(&vm_id)
            .map_or(0, |guest| guest.pending.len());
        let mut output = Vec::with_capacity(pending.saturating_add(1));
        if self.physical_line_open && self.owner != Some(vm_id) {
            output.push(b'\n');
        }
        self.physical_line_open = false;
        self.owner = Some(vm_id);

        let guest = self.guests.entry(vm_id).or_default();
        self.total_pending -= guest.pending.len();
        output.extend(guest.pending.drain(..));
        self.update_physical_line(vm_id, &output);
        output
    }

    fn update_physical_line(&mut self, vm_id: usize, output: &[u8]) {
        let guest = self.guests.entry(vm_id).or_default();
        for &byte in output {
            guest.at_line_start = byte == b'\n';
            self.physical_line_open = byte != b'\n';
        }
        if self.physical_line_open {
            self.owner = Some(vm_id);
        } else {
            self.owner = None;
        }
    }

    fn append_log(&mut self, vm_id: usize, bytes: &[u8]) {
        let guest = self.guests.entry(vm_id).or_default();
        for &byte in bytes {
            if guest.pending.len() == PER_GUEST_LOG_CAPACITY {
                guest.pending.pop_front();
                self.total_pending -= 1;
            }
            guest.pending.push_back(byte);
            self.total_pending += 1;
        }
    }

    #[cfg(test)]
    fn pending_len(&self, vm_id: usize) -> usize {
        self.guests
            .get(&vm_id)
            .map_or(0, |guest| guest.pending.len())
    }

    #[cfg(test)]
    fn total_pending(&self) -> usize {
        self.total_pending
    }

    #[cfg(test)]
    fn preemption(&self) -> Option<usize> {
        self.preemption
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> ArbitrationSnapshot {
        ArbitrationSnapshot {
            mode: self.mode,
            owner: self.owner,
            physical_line_open: self.physical_line_open,
            preemption: self.preemption,
            total_pending: self.total_pending,
            guests: self
                .guests
                .iter()
                .map(|(&vm_id, guest)| (vm_id, guest.pending.len(), guest.at_line_start))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn complete_line_is_emitted_while_other_fragment_remains_buffered() {
        let mut mux = GuestOutputMux::default();
        let mut host = Vec::new();

        host.extend(mux.format(2, true, b"booting"));
        host.extend(mux.format(1, true, b"ready"));
        host.extend(mux.format(2, true, b" linux\n"));

        assert_eq!(host, b"[VM 2] booting linux\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn complete_line_preempts_an_abandoned_partial_fragment() {
        let mut mux = GuestOutputMux::default();

        assert!(mux.format(2, true, b"~ # ").is_empty());
        assert_eq!(mux.format(1, true, b"ready\n"), b"[VM 1] ready\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn competing_guest_does_not_split_an_incomplete_logical_line() {
        let mut mux = GuestOutputMux::default();

        assert!(mux.format(1, true, b"long ").is_empty());
        assert_eq!(mux.format(2, true, b"ready\n"), b"[VM 2] ready\n");
        assert_eq!(
            mux.format(1, true, b"logical line\n"),
            b"[VM 1] long logical line\n"
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn fragmented_output_from_one_guest_keeps_one_prefix() {
        let mut mux = GuestOutputMux::default();

        assert!(mux.format(1, true, b"prom").is_empty());
        assert_eq!(
            mux.format(1, true, b"pt\nnext\n"),
            b"[VM 1] prompt\n[VM 1] next\n"
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn pending_output_is_bounded_per_guest() {
        let mut mux = GuestOutputMux::default();
        assert!(mux.format(0, true, b"open").is_empty());
        assert!(
            mux.format(1, true, &vec![b'x'; PER_GUEST_LOG_CAPACITY + 1])
                .is_empty()
        );

        assert_eq!(mux.pending_len(1), PER_GUEST_LOG_CAPACITY);
        assert_eq!(mux.format(0, true, b"\n"), b"[VM 0] open\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn full_boot_ring_retains_the_newline_that_completes_a_line() {
        let mut mux = GuestOutputMux::default();
        assert!(
            mux.format(1, true, &vec![b'x'; PER_GUEST_LOG_CAPACITY])
                .is_empty()
        );

        let output = mux.format(1, true, b"\n");

        assert!(output.starts_with(b"[VM 1] "));
        assert!(output.ends_with(b"\n"));
        assert_eq!(
            output.iter().filter(|&&byte| byte == b'x').count(),
            PER_GUEST_LOG_CAPACITY - 1
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn oversized_backend_write_does_not_drive_output_allocation() {
        let mut mux = GuestOutputMux::default();
        let output = mux.format(1, true, &vec![b'x'; PER_GUEST_LOG_CAPACITY * 4]);

        assert!(output.capacity() <= PER_GUEST_LOG_CAPACITY + 16);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn total_pending_output_is_bounded_and_cleanup_releases_capacity() {
        let mut mux = GuestOutputMux::default();
        assert!(mux.format(0, true, b"open").is_empty());

        for vm_id in 1..=5 {
            assert!(
                mux.format(vm_id, true, &vec![b'x'; PER_GUEST_LOG_CAPACITY])
                    .is_empty()
            );
        }
        assert_eq!(mux.total_pending(), PER_GUEST_LOG_CAPACITY * 5 + 4);

        mux.reset_guest(1);
        assert_eq!(mux.total_pending(), PER_GUEST_LOG_CAPACITY * 4 + 4);

        mux.reconcile_running(&BTreeSet::from([0, 5]));
        assert_eq!(mux.total_pending(), PER_GUEST_LOG_CAPACITY + 4);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn complete_line_preempts_a_background_fragment_once() {
        let mut mux = GuestOutputMux::default();
        assert!(mux.format(2, true, b"~ # ").is_empty());

        mux.request_preemption(1);
        assert_eq!(mux.format(1, true, b"echo ok\n"), b"[VM 1] echo ok\n");

        assert!(mux.format(2, true, b"next").is_empty());
        assert_eq!(mux.format(1, true, b"queued\n"), b"[VM 1] queued\n");
        assert_eq!(mux.format(2, true, b" line\n"), b"[VM 2] ~ # next line\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn interactive_background_ring_overwrites_oldest_bytes_before_replay() {
        let mut mux = GuestOutputMux::default();
        mux.enter_interactive(1);
        assert!(
            mux.format(2, true, &vec![b'a'; PER_GUEST_LOG_CAPACITY])
                .is_empty()
        );
        assert!(mux.format(2, true, b"TAIL").is_empty());

        let mut expected = vec![b'a'; PER_GUEST_LOG_CAPACITY - 4];
        expected.extend_from_slice(b"TAIL");
        assert_eq!(mux.select_foreground(2), expected);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn interactive_foreground_fragments_stay_on_the_same_physical_line() {
        let mut mux = GuestOutputMux::default();
        mux.enter_interactive(1);

        assert_eq!(mux.format(1, true, b"e"), b"e");
        assert_eq!(mux.format(1, true, b"c"), b"c");
        assert_eq!(mux.format(1, true, b"ho\n"), b"ho\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn buffering_for_the_shell_closes_guest_line_ownership() {
        let mut mux = GuestOutputMux::default();
        mux.enter_interactive(1);
        assert_eq!(mux.format(1, true, b"~ # "), b"~ # ");

        assert_eq!(mux.buffer_all(), b"\n");
        assert!(mux.format(2, true, b"cached\n").is_empty());

        assert_eq!(mux.select_foreground(2), b"cached\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn resetting_an_open_line_owner_preserves_a_physical_separator() {
        let mut mux = GuestOutputMux::default();
        assert!(mux.format(2, true, b"partial").is_empty());

        mux.reset_guest(2);

        assert_eq!(mux.format(1, true, b"ready\n"), b"[VM 1] ready\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn reconciliation_clears_invalid_preemption_and_preserves_separator() {
        let mut mux = GuestOutputMux::default();
        assert!(mux.format(2, true, b"partial").is_empty());
        mux.request_preemption(2);

        mux.reconcile_running(&BTreeSet::from([1]));

        assert_eq!(mux.preemption(), None);
        assert_eq!(mux.format(1, true, b"ready\n"), b"[VM 1] ready\n");
    }
}
