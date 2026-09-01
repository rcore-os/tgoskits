//! Line-safe arbitration for output from multiple guest consoles.

use alloc::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    vec::Vec,
};

pub const PER_GUEST_LOG_CAPACITY: usize = 16 * 1024;

/// Arbitrates complete host-console lines across guest serial backends.
#[derive(Debug, Default)]
pub struct GuestOutputMux {
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
    dropped_bytes: usize,
    at_line_start: bool,
}

#[cfg(any(test, axtest))]
#[derive(Debug, Eq, PartialEq)]
pub struct ArbitrationSnapshot {
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

    /// Starts the boot-time mode that displays complete lines from every VM.
    pub fn start_boot_multiplex(&mut self) {
        self.mode = OutputMode::BootMultiplex;
    }

    /// Gives one interactive guest direct access to the host console.
    pub fn enter_interactive(&mut self, vm_id: usize) {
        self.mode = OutputMode::Interactive {
            foreground: Some(vm_id),
        };
    }

    pub fn foreground_is_interactive(&self) -> bool {
        matches!(
            self.mode,
            OutputMode::Interactive {
                foreground: Some(_)
            }
        )
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
        output.extend_from_slice(record);
        if !record_ends_with_line_break(record) {
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
    pub fn select_foreground(&mut self, vm_id: usize) -> Vec<u8> {
        self.enter_interactive(vm_id);
        self.drain_guest_log(vm_id)
    }

    /// Enters interactive mode on the first input and returns the buffered prompt.
    pub fn select_foreground_on_input(&mut self, vm_id: usize) -> Vec<u8> {
        match self.mode {
            OutputMode::Interactive {
                foreground: Some(foreground),
            } if foreground == vm_id => Vec::new(),
            _ => self.select_foreground(vm_id),
        }
    }

    /// Discards output state for guests that are no longer running.
    pub fn reconcile_running(&mut self, running: &BTreeSet<usize>) {
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
    pub fn reset_guest(&mut self, vm_id: usize) {
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
    pub fn request_preemption(&mut self, vm_id: usize) {
        self.preemption = Some(vm_id);
    }

    /// Enqueues one backend write and returns bytes ready for the host console.
    pub fn format(&mut self, vm_id: usize, multiple_running: bool, bytes: &[u8]) -> Vec<u8> {
        self.register_guest(vm_id);
        let mut output = Vec::new();
        let formatted = self.format_registered_into(vm_id, multiple_running, bytes, &mut |bytes| {
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
        multiple_running: bool,
        bytes: &[u8],
        emit: &mut dyn FnMut(&[u8]),
    ) -> bool {
        if !self.guests.contains_key(&vm_id) {
            return false;
        }
        if let OutputMode::Interactive { foreground } = self.mode {
            self.format_interactive_into(vm_id, foreground, bytes, emit);
            return true;
        }

        self.append_registered_log(vm_id, bytes);

        if !multiple_running {
            let mut emitted_separator = false;
            if self.physical_line_open && self.owner != Some(vm_id) {
                emit(b"\n");
                self.physical_line_open = false;
                emitted_separator = true;
            }
            self.owner = Some(vm_id);
            let guest = self
                .guests
                .get_mut(&vm_id)
                .expect("registered guest output state disappeared");
            let dropped_bytes = core::mem::take(&mut guest.dropped_bytes);
            let pending_len = guest.pending.len();
            let last = guest.pending.back().copied();
            let (first, second) = guest.pending.as_slices();
            emit_guest_drop_summary(vm_id, dropped_bytes, emit);
            emit(first);
            emit(second);
            guest.pending.clear();
            self.total_pending -= pending_len;

            if let Some(last) = last {
                guest.at_line_start = last == b'\n';
                self.physical_line_open = last != b'\n';
            } else if emitted_separator {
                guest.at_line_start = true;
                self.physical_line_open = false;
            }
            if !self.physical_line_open || last.is_none() {
                self.owner = None;
            }
            return true;
        }

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
                emit(b"\n");
                self.physical_line_open = false;
            }
            if let Some(previous_owner) = self.owner
                && let Some(previous_guest) = self.guests.get_mut(&previous_owner)
            {
                previous_guest.at_line_start = true;
            }
            self.owner = Some(next);
            let dropped_bytes = self
                .guests
                .get_mut(&next)
                .map(|guest| core::mem::take(&mut guest.dropped_bytes))
                .unwrap_or(0);
            emit_guest_drop_summary(next, dropped_bytes, emit);
            emit_vm_prefix(next, emit);

            let guest = self
                .guests
                .get_mut(&next)
                .expect("completed line must have guest state");
            let line_len = guest
                .pending
                .iter()
                .position(|&byte| byte == b'\n')
                .expect("completed line was selected without a newline")
                + 1;
            let (first, second) = guest.pending.as_slices();
            let first_len = line_len.min(first.len());
            emit(&first[..first_len]);
            if line_len > first_len {
                emit(&second[..line_len - first_len]);
            }
            guest.pending.drain(..line_len);
            guest.at_line_start = true;
            self.total_pending -= line_len;
            self.physical_line_open = false;
            self.owner = None;
        }

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
    fn pending_len(&self, vm_id: usize) -> usize {
        self.guests
            .get(&vm_id)
            .map_or(0, |guest| guest.pending.len())
    }

    #[cfg(any(test, axtest))]
    fn total_pending(&self) -> usize {
        self.total_pending
    }

    #[cfg(any(test, axtest))]
    fn preemption(&self) -> Option<usize> {
        self.preemption
    }

    #[cfg(any(test, axtest))]
    pub fn snapshot(&self) -> ArbitrationSnapshot {
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

fn record_ends_with_line_break(mut record: &[u8]) -> bool {
    loop {
        if record.ends_with(b"\n") {
            return true;
        }
        let Some(sgr_start) = trailing_sgr_start(record) else {
            return false;
        };
        record = &record[..sgr_start];
    }
}

fn trailing_sgr_start(record: &[u8]) -> Option<usize> {
    if record.last() != Some(&b'm') {
        return None;
    }
    let start = record.iter().rposition(|&byte| byte == b'\x1b')?;
    if record.get(start + 1) != Some(&b'[')
        || !record[start + 2..record.len() - 1]
            .iter()
            .all(|byte| (0x30..=0x3f).contains(byte))
    {
        return None;
    }
    Some(start)
}

fn emit_vm_prefix(vm_id: usize, emit: &mut dyn FnMut(&[u8])) {
    emit(b"[VM ");

    emit_usize(vm_id, emit);
    emit(b"] ");
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

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn host_record_terminates_an_open_guest_line() {
        let mut mux = GuestOutputMux::default();
        assert_eq!(mux.format(1, false, b"guest> "), b"guest> ");

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
    fn host_record_with_trailing_sgr_does_not_add_a_blank_line() {
        let mut mux = GuestOutputMux::default();
        let record = b"\x1b[37mhost record\x1b[m\r\n\x1b[m";

        assert_eq!(mux.format_host_record(record), record);
    }

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

        assert!(output.starts_with(b"[Axvisor VM 1 console dropped 1 buffered bytes]\n[VM 1] "));
        assert!(output.ends_with(b"\n"));
        let guest_line = output
            .windows(b"[VM 1] ".len())
            .position(|window| window == b"[VM 1] ")
            .map(|offset| &output[offset + b"[VM 1] ".len()..])
            .expect("guest line prefix");
        assert_eq!(
            guest_line.iter().filter(|&&byte| byte == b'x').count(),
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
