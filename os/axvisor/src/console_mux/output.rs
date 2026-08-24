//! Line-safe arbitration for output from multiple guest consoles.

use alloc::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    vec::Vec,
};

pub const PER_GUEST_LOG_CAPACITY: usize = 16 * 1024;

/// Emits terminal text with every bare line feed expanded to CRLF.
pub fn emit_text_with_crlf(bytes: &[u8], emit: &mut dyn FnMut(&[u8])) {
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte != b'\n' {
            continue;
        }
        if index > 0 && bytes[index - 1] == b'\r' {
            emit(&bytes[start..=index]);
        } else {
            emit(&bytes[start..index]);
            emit(b"\r\n");
        }
        start = index + 1;
    }
    emit(&bytes[start..]);
}

/// Arbitrates complete host-console lines across guest serial backends.
#[derive(Debug, Default)]
pub struct GuestOutputMux {
    guests: BTreeMap<usize, GuestOutputState>,
    foreground: Option<usize>,
    owner: Option<usize>,
    physical_line_open: bool,
    total_pending: usize,
}

#[derive(Debug)]
struct GuestOutputState {
    pending: VecDeque<u8>,
    dropped_bytes: usize,
    at_line_start: bool,
}

#[cfg(any(test, axtest))]
#[derive(Debug, Eq, PartialEq)]
pub struct ArbitrationSnapshot {
    foreground: Option<usize>,
    owner: Option<usize>,
    physical_line_open: bool,
    total_pending: usize,
    guests: Vec<(usize, usize, bool)>,
}

impl Default for GuestOutputState {
    fn default() -> Self {
        Self {
            pending: VecDeque::with_capacity(PER_GUEST_LOG_CAPACITY),
            dropped_bytes: 0,
            at_line_start: true,
        }
    }
}

impl GuestOutputMux {
    /// Prepares bounded output state before a guest backend can start writing.
    ///
    /// [`Self::format_registered_into`] does not allocate, so callers that use
    /// it from an atomic device callback must register the guest in task
    /// context first.
    pub fn register_guest(&mut self, vm_id: usize) {
        self.guests.entry(vm_id).or_default();
    }

    /// Gives one interactive guest direct access to the host console.
    pub fn enter_interactive(&mut self, vm_id: usize) {
        self.foreground = Some(vm_id);
    }

    pub fn foreground_is_interactive(&self) -> bool {
        self.foreground.is_some()
    }

    /// Emits one host record without allowing it to share a guest line.
    pub fn format_host_record(&mut self, record: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(record.len().saturating_add(2));
        if self.physical_line_open {
            output.push(b'\n');
        }
        if let Some(owner) = self.owner
            && let Some(guest) = self.guests.get_mut(&owner)
        {
            guest.at_line_start = true;
        }
        self.owner = None;
        self.physical_line_open = false;
        if let Some((line_break_start, suffix_start)) = trailing_sgr_after_line_break(record) {
            output.extend_from_slice(&record[..line_break_start]);
            output.extend_from_slice(&record[suffix_start..]);
            output.extend_from_slice(&record[line_break_start..suffix_start]);
        } else {
            output.extend_from_slice(record);
            output.push(b'\n');
        }
        output
    }

    /// Keeps every guest's output in its ring and terminates its open physical line.
    pub fn buffer_all(&mut self) -> Vec<u8> {
        let separator = if self.physical_line_open {
            Vec::from(*b"\n")
        } else {
            Vec::new()
        };
        self.foreground = None;
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
    pub fn select_foreground(&mut self, vm_id: usize) -> Vec<u8> {
        self.enter_interactive(vm_id);
        self.drain_guest_log(vm_id)
    }

    /// Enters interactive mode on the first input and returns the buffered prompt.
    pub fn select_foreground_on_input(&mut self, vm_id: usize) -> Vec<u8> {
        if self.foreground == Some(vm_id) {
            Vec::new()
        } else {
            self.select_foreground(vm_id)
        }
    }

    /// Removes output state for guests that are no longer running.
    ///
    /// When the management shell owns the console, buffered output is returned
    /// before removal. This closes the race where a short-lived guest exits
    /// after producing its completion marker but before the shell attaches to
    /// it. Output from a background guest remains hidden while another guest is
    /// attached.
    pub fn reconcile_running(&mut self, running: &BTreeSet<usize>) -> Vec<u8> {
        let stopped = self
            .guests
            .keys()
            .filter(|vm_id| !running.contains(vm_id))
            .copied()
            .collect::<Vec<_>>();
        let replay_stopped = self.foreground.is_none();
        let mut replay = Vec::new();
        for vm_id in stopped {
            if replay_stopped {
                replay.extend(self.drain_guest_log(vm_id));
            }
            self.reset_guest(vm_id);
        }
        replay
    }

    /// Discards pending output for a replaced or stopped backend.
    pub fn reset_guest(&mut self, vm_id: usize) {
        if let Some(guest) = self.guests.remove(&vm_id) {
            self.total_pending -= guest.pending.len();
        }
        if self.owner == Some(vm_id) {
            self.owner = None;
        }
    }

    /// Enqueues one backend write and returns bytes ready for the host console.
    pub fn format(&mut self, vm_id: usize, bytes: &[u8]) -> Vec<u8> {
        self.register_guest(vm_id);
        let mut output = Vec::new();
        let formatted = self.format_registered_into(vm_id, bytes, &mut |bytes| {
            output.extend_from_slice(bytes);
        });
        debug_assert!(formatted, "the guest was registered immediately above");
        output
    }

    /// Formats one write from a previously registered guest into `emit`.
    ///
    /// This path neither allocates nor sleeps. The callback may therefore
    /// append directly to a fixed-capacity transport while the caller holds a
    /// vCPU or device-context guard. Returns `false` for a stale or otherwise
    /// unregistered backend.
    pub fn format_registered_into(
        &mut self,
        vm_id: usize,
        bytes: &[u8],
        emit: &mut dyn FnMut(&[u8]),
    ) -> bool {
        if !self.guests.contains_key(&vm_id) {
            return false;
        }
        self.format_interactive_into(vm_id, self.foreground, bytes, emit);
        true
    }

    fn format_interactive_into(
        &mut self,
        vm_id: usize,
        foreground: Option<usize>,
        bytes: &[u8],
        emit: &mut dyn FnMut(&[u8]),
    ) {
        if foreground != Some(vm_id) {
            self.append_registered_log(vm_id, bytes);
            return;
        }

        self.drain_registered_guest_log_into(vm_id, emit);
        emit(bytes);
        self.update_physical_line(vm_id, bytes);
    }

    fn drain_registered_guest_log_into(&mut self, vm_id: usize, emit: &mut dyn FnMut(&[u8])) {
        let emitted_separator = self.physical_line_open && self.owner != Some(vm_id);
        if emitted_separator {
            emit(b"\n");
        }
        self.physical_line_open = false;
        self.owner = Some(vm_id);

        let guest = self
            .guests
            .get_mut(&vm_id)
            .expect("registered guest output state disappeared");
        let dropped_bytes = core::mem::take(&mut guest.dropped_bytes);
        let pending_len = guest.pending.len();
        let pending_last = guest.pending.back().copied();
        let (first, second) = guest.pending.as_slices();
        emit_guest_drop_summary(vm_id, dropped_bytes, emit);
        emit(first);
        emit(second);
        guest.pending.clear();
        self.total_pending -= pending_len;

        match pending_last.or((emitted_separator || dropped_bytes != 0).then_some(b'\n')) {
            Some(last) => {
                guest.at_line_start = last == b'\n';
                self.physical_line_open = last != b'\n';
            }
            None => self.physical_line_open = false,
        }
        if !self.physical_line_open {
            self.owner = None;
        }
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
        let dropped_bytes = core::mem::take(&mut guest.dropped_bytes);
        self.total_pending -= guest.pending.len();
        emit_guest_drop_summary(vm_id, dropped_bytes, &mut |bytes| {
            output.extend_from_slice(bytes)
        });
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

    fn append_registered_log(&mut self, vm_id: usize, bytes: &[u8]) {
        let guest = self
            .guests
            .get_mut(&vm_id)
            .expect("registered guest output state disappeared");
        debug_assert!(guest.pending.capacity() >= PER_GUEST_LOG_CAPACITY);
        for &byte in bytes {
            if guest.pending.len() == PER_GUEST_LOG_CAPACITY {
                guest.pending.pop_front();
                self.total_pending -= 1;
                guest.dropped_bytes = guest.dropped_bytes.saturating_add(1);
            }
            guest.pending.push_back(byte);
            self.total_pending += 1;
        }
    }

    #[cfg(any(test, axtest))]
    pub fn pending_len(&self, vm_id: usize) -> usize {
        self.guests
            .get(&vm_id)
            .map_or(0, |guest| guest.pending.len())
    }

    #[cfg(any(test, axtest))]
    fn total_pending(&self) -> usize {
        self.total_pending
    }

    #[cfg(any(test, axtest))]
    pub fn snapshot(&self) -> ArbitrationSnapshot {
        ArbitrationSnapshot {
            foreground: self.foreground,
            owner: self.owner,
            physical_line_open: self.physical_line_open,
            total_pending: self.total_pending,
            guests: self
                .guests
                .iter()
                .map(|(&vm_id, guest)| (vm_id, guest.pending.len(), guest.at_line_start))
                .collect(),
        }
    }
}

fn trailing_sgr_after_line_break(record: &[u8]) -> Option<(usize, usize)> {
    let line_break = record.iter().rposition(|&byte| byte == b'\n')?;
    let suffix_start = line_break + 1;
    let mut suffix = &record[suffix_start..];
    while !suffix.is_empty() {
        if !suffix.starts_with(b"\x1b[") {
            return None;
        }
        let end = suffix[2..].iter().position(|&byte| byte == b'm')?;
        if !suffix[2..end + 2]
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b';' | b':'))
        {
            return None;
        }
        suffix = &suffix[end + 3..];
    }
    let line_break_start = line_break
        .checked_sub(1)
        .filter(|&index| record[index] == b'\r')
        .unwrap_or(line_break);
    Some((line_break_start, suffix_start))
}

fn emit_guest_drop_summary(vm_id: usize, dropped_bytes: usize, emit: &mut dyn FnMut(&[u8])) {
    if dropped_bytes == 0 {
        return;
    }
    emit(b"[Axvisor VM ");
    emit_usize(vm_id, emit);
    emit(b" console dropped ");
    emit_usize(dropped_bytes, emit);
    emit(b" buffered bytes]\n");
}

fn emit_usize(value: usize, emit: &mut dyn FnMut(&[u8])) {
    let mut digits = [0; usize::MAX.ilog10() as usize + 1];
    let mut cursor = digits.len();
    let mut value = value;
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    emit(&digits[cursor..]);
}

#[cfg(any(test, axtest))]
mod tests {
    use super::*;

    #[test]
    fn host_text_expands_bare_lf_without_doubling_crlf() {
        let mut output = Vec::new();

        emit_text_with_crlf(b"first\nsecond\r\nthird", &mut |bytes| {
            output.extend_from_slice(bytes);
        });

        assert_eq!(output, b"first\r\nsecond\r\nthird");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn host_record_terminates_an_open_guest_line() {
        let mut mux = GuestOutputMux::default();
        mux.enter_interactive(1);
        assert_eq!(mux.format(1, b"guest> "), b"guest> ");

        assert_eq!(mux.format_host_record(b"host record\n"), b"\nhost record\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn host_record_without_newline_becomes_an_independent_line() {
        let mut mux = GuestOutputMux::default();
        assert_eq!(mux.format_host_record(b":host"), b":host\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn host_record_moves_trailing_ansi_reset_before_line_break() {
        let mut mux = GuestOutputMux::default();
        let record = b"\x1b[37mhost record\x1b[m\r\n\x1b[m";

        assert_eq!(
            mux.format_host_record(record),
            b"\x1b[37mhost record\x1b[m\x1b[m\r\n"
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn pending_output_is_bounded_per_guest() {
        let mut mux = GuestOutputMux::default();
        assert!(
            mux.format(1, &vec![b'x'; PER_GUEST_LOG_CAPACITY + 1])
                .is_empty()
        );

        assert_eq!(mux.pending_len(1), PER_GUEST_LOG_CAPACITY);
        let replay = mux.select_foreground(1);
        assert!(replay.starts_with(b"[Axvisor VM 1 console dropped 1 buffered bytes]\n"));
        let payload_start = replay
            .iter()
            .position(|&byte| byte == b'\n')
            .expect("drop summary must end with a newline")
            + 1;
        let payload = &replay[payload_start..];
        assert_eq!(payload, vec![b'x'; PER_GUEST_LOG_CAPACITY]);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn oversized_backend_write_does_not_drive_output_allocation() {
        let mut mux = GuestOutputMux::default();
        let output = mux.format(1, &vec![b'x'; PER_GUEST_LOG_CAPACITY * 4]);

        assert!(output.capacity() <= PER_GUEST_LOG_CAPACITY + 16);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn total_pending_output_is_bounded_and_cleanup_releases_capacity() {
        let mut mux = GuestOutputMux::default();
        assert!(mux.format(0, b"open").is_empty());

        for vm_id in 1..=5 {
            assert!(
                mux.format(vm_id, &vec![b'x'; PER_GUEST_LOG_CAPACITY])
                    .is_empty()
            );
        }
        assert_eq!(mux.total_pending(), PER_GUEST_LOG_CAPACITY * 5 + 4);

        mux.reset_guest(1);
        assert_eq!(mux.total_pending(), PER_GUEST_LOG_CAPACITY * 4 + 4);

        let _ = mux.reconcile_running(&BTreeSet::from([0, 5]));
        assert_eq!(mux.total_pending(), PER_GUEST_LOG_CAPACITY + 4);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn reconciliation_replays_a_stopped_guests_shell_buffer() {
        let mut mux = GuestOutputMux::default();
        mux.buffer_all();
        mux.register_guest(1);
        assert!(mux.format(1, b"ARCEOS_VIRTIO_BLK_PASS\n").is_empty());

        assert_eq!(
            mux.reconcile_running(&BTreeSet::new()),
            b"ARCEOS_VIRTIO_BLK_PASS\n"
        );
        assert_eq!(mux.total_pending(), 0);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn reconciliation_does_not_replay_background_output_into_an_attached_guest() {
        let mut mux = GuestOutputMux::default();
        mux.enter_interactive(2);
        mux.register_guest(1);
        mux.register_guest(2);
        assert!(mux.format(1, b"background\n").is_empty());

        assert!(mux.reconcile_running(&BTreeSet::from([2])).is_empty());
        assert_eq!(mux.total_pending(), 0);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn interactive_background_ring_overwrites_oldest_bytes_before_replay() {
        let mut mux = GuestOutputMux::default();
        mux.enter_interactive(1);
        assert!(
            mux.format(2, &vec![b'a'; PER_GUEST_LOG_CAPACITY])
                .is_empty()
        );
        assert!(mux.format(2, b"TAIL").is_empty());

        let mut expected = b"[Axvisor VM 2 console dropped 4 buffered bytes]\n".to_vec();
        expected.extend(vec![b'a'; PER_GUEST_LOG_CAPACITY - 4]);
        expected.extend_from_slice(b"TAIL");
        assert_eq!(mux.select_foreground(2), expected);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn interactive_foreground_fragments_stay_on_the_same_physical_line() {
        let mut mux = GuestOutputMux::default();
        mux.enter_interactive(1);

        assert_eq!(mux.format(1, b"e"), b"e");
        assert_eq!(mux.format(1, b"c"), b"c");
        assert_eq!(mux.format(1, b"ho\n"), b"ho\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn buffering_for_the_shell_closes_guest_line_ownership() {
        let mut mux = GuestOutputMux::default();
        mux.enter_interactive(1);
        assert_eq!(mux.format(1, b"~ # "), b"~ # ");

        assert_eq!(mux.buffer_all(), b"\n");
        assert!(mux.format(2, b"cached\n").is_empty());

        assert_eq!(mux.select_foreground(2), b"cached\n");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn resetting_an_open_line_owner_preserves_a_physical_separator() {
        let mut mux = GuestOutputMux::default();
        mux.enter_interactive(2);
        assert_eq!(mux.format(2, b"partial"), b"partial");

        mux.reset_guest(2);
        mux.enter_interactive(1);

        assert_eq!(mux.format(1, b"ready\n"), b"\nready\n");
    }
}
