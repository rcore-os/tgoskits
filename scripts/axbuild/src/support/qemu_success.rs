use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use regex_automata::{
    Input,
    dfa::{Automaton, dense},
    util::primitives::StateID,
};

const TRANSCRIPT_TAIL_BYTES: usize = 2048;

#[derive(Clone)]
pub(crate) struct QemuSuccessOutput {
    state: Arc<Mutex<QemuSuccessOutputState>>,
}

impl QemuSuccessOutput {
    fn new(success_regex: &[String]) -> Self {
        Self {
            state: Arc::new(Mutex::new(QemuSuccessOutputState {
                matcher: StreamingMatcher::new(success_regex),
                tail: Vec::with_capacity(TRANSCRIPT_TAIL_BYTES),
            })),
        }
    }

    pub(crate) fn append(&self, chunk: &[u8]) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.append(chunk);
    }

    fn snapshot(&self) -> QemuSuccessSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        QemuSuccessSnapshot {
            matched: state.matcher.as_ref().is_ok_and(StreamingMatcher::is_match),
            matcher_error: state.matcher.as_ref().err().cloned(),
            tail: state.tail.clone(),
        }
    }
}

struct QemuSuccessOutputState {
    matcher: std::result::Result<StreamingMatcher, String>,
    tail: Vec<u8>,
}

impl QemuSuccessOutputState {
    fn append(&mut self, chunk: &[u8]) {
        if let Ok(matcher) = &mut self.matcher {
            matcher.append(chunk);
        }
        append_bounded_tail(&mut self.tail, chunk);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnsiEscapeState {
    Text,
    Escape,
    Csi,
}

struct StreamingMatcher {
    dfa: dense::DFA<Vec<u32>>,
    state: StateID,
    ansi_state: AnsiEscapeState,
    matched: bool,
}

impl StreamingMatcher {
    fn new(patterns: &[String]) -> std::result::Result<Self, String> {
        let dfa = dense::Builder::new()
            .build_many(patterns)
            .map_err(|err| err.to_string())?;
        let state = dfa
            .start_state_forward(&Input::new(b""))
            .map_err(|err| err.to_string())?;
        Ok(Self {
            dfa,
            state,
            ansi_state: AnsiEscapeState::Text,
            matched: false,
        })
    }

    fn append(&mut self, chunk: &[u8]) {
        if self.matched {
            return;
        }

        for &byte in chunk {
            self.append_byte(byte);
            if self.matched {
                return;
            }
        }
    }

    fn append_byte(&mut self, byte: u8) {
        match self.ansi_state {
            AnsiEscapeState::Text => self.append_text_byte(byte),
            AnsiEscapeState::Escape if byte == b'[' => {
                self.ansi_state = AnsiEscapeState::Csi;
            }
            AnsiEscapeState::Escape => {
                self.ansi_state = AnsiEscapeState::Text;
                self.advance_dfa(0x1b);
                if !self.matched {
                    self.append_text_byte(byte);
                }
            }
            AnsiEscapeState::Csi if (0x40..=0x7e).contains(&byte) => {
                self.ansi_state = AnsiEscapeState::Text;
            }
            AnsiEscapeState::Csi => {}
        }
    }

    fn append_text_byte(&mut self, byte: u8) {
        if byte == 0x1b {
            self.ansi_state = AnsiEscapeState::Escape;
        } else {
            self.advance_dfa(byte);
        }
    }

    fn advance_dfa(&mut self, byte: u8) {
        self.state = self.dfa.next_state(self.state, byte);
        self.matched = self.dfa.is_match_state(self.state);
    }

    fn is_match(&self) -> bool {
        if self.matched {
            return true;
        }

        let mut state = self.state;
        if self.ansi_state == AnsiEscapeState::Escape {
            state = self.dfa.next_state(state, 0x1b);
            if self.dfa.is_match_state(state) {
                return true;
            }
        }

        state = self.dfa.next_eoi_state(state);
        self.dfa.is_match_state(state)
    }
}

struct QemuSuccessSnapshot {
    matched: bool,
    matcher_error: Option<String>,
    tail: Vec<u8>,
}

fn append_bounded_tail(tail: &mut Vec<u8>, chunk: &[u8]) {
    if chunk.len() >= TRANSCRIPT_TAIL_BYTES {
        tail.clear();
        tail.extend_from_slice(&chunk[chunk.len() - TRANSCRIPT_TAIL_BYTES..]);
        return;
    }

    let overflow = tail
        .len()
        .saturating_add(chunk.len())
        .saturating_sub(TRANSCRIPT_TAIL_BYTES);
    if overflow > 0 {
        tail.drain(..overflow);
    }
    tail.extend_from_slice(chunk);
}

pub(crate) fn capture_required_success_output(
    success_regex: &[String],
    capture: Option<crate::backtrace::BacktraceQemuCapture>,
) -> (
    Option<crate::backtrace::BacktraceQemuCapture>,
    Option<QemuSuccessOutput>,
) {
    if success_regex.is_empty() {
        return (capture, None);
    }

    let success_output = QemuSuccessOutput::new(success_regex);
    let capture = match capture {
        Some(capture) => capture.with_success_output(success_output.clone()),
        None => crate::backtrace::BacktraceQemuCapture::success_output_only(success_output.clone()),
    };
    (Some(capture), Some(success_output))
}

const BENIGN_QEMU_STOP_ERROR: &str = "QEMU stopped without matching a configured success regex";

fn is_benign_qemu_stop_error(err: &anyhow::Error) -> bool {
    let message = err.to_string();
    if message == BENIGN_QEMU_STOP_ERROR {
        return true;
    }

    message
        .strip_prefix(BENIGN_QEMU_STOP_ERROR)
        .and_then(|suffix| suffix.strip_prefix("; transcript tail:"))
        .is_some_and(|transcript| transcript.is_empty() || transcript.starts_with('\n'))
}
pub(crate) fn verify_qemu_success_contract(
    run_result: Result<()>,
    success_output: Option<&QemuSuccessOutput>,
) -> Result<()> {
    let Some(success_output) = success_output else {
        return run_result;
    };

    let run_error = match run_result {
        Ok(()) => None,
        Err(err) if is_benign_qemu_stop_error(&err) => None,
        Err(err) => Some(err),
    };

    if let Some(err) = run_error {
        return Err(err);
    }

    let snapshot = success_output.snapshot();
    if let Some(err) = snapshot.matcher_error {
        bail!("failed to compile QEMU success regex set: {err}");
    }
    if snapshot.matched {
        return Ok(());
    }

    let tail = String::from_utf8_lossy(&snapshot.tail);
    bail!(
        "QEMU stopped without matching a configured success regex; transcript tail:
{tail}"
    )
}
#[cfg(test)]
mod tests {
    use super::{QemuSuccessOutput, TRANSCRIPT_TAIL_BYTES, verify_qemu_success_contract};

    fn captured_output(patterns: &[&str], chunks: &[&[u8]]) -> QemuSuccessOutput {
        let patterns = patterns.iter().map(ToString::to_string).collect::<Vec<_>>();
        let output = QemuSuccessOutput::new(&patterns);
        for chunk in chunks {
            output.append(chunk);
        }
        output
    }

    #[test]
    fn exit_zero_without_required_success_marker_is_an_error() {
        let output = captured_output(
            &[r"(?m)^STARRY_GROUPED_TESTS_PASSED\s*$"],
            &[b"guest exited before completing the suite\n"],
        );
        let err = verify_qemu_success_contract(Ok(()), Some(&output)).unwrap_err();

        assert!(
            err.to_string()
                .contains("without matching a configured success regex")
        );
    }

    #[test]
    fn configured_success_marker_is_accepted() {
        let output = captured_output(
            &[r"(?m)^STARRY_GROUPED_TESTS_PASSED\s*$"],
            &[b"booting\nSTARRY_GROUPED_TESTS_PASSED\n"],
        );
        verify_qemu_success_contract(Ok(()), Some(&output)).unwrap();
    }

    #[test]
    fn empty_success_contract_preserves_normal_exit() {
        verify_qemu_success_contract(Ok(()), None).unwrap();
    }

    #[test]
    fn benign_stop_error_is_accepted_when_success_marker_was_captured() {
        let output = captured_output(&["PASS"], &[b"PASS\n"]);
        verify_qemu_success_contract(
            Err(anyhow::anyhow!(
                "QEMU stopped without matching a configured success regex; transcript tail:"
            )),
            Some(&output),
        )
        .unwrap();
    }
    #[test]
    fn extended_benign_stop_error_is_not_ignored() {
        let output = captured_output(&["PASS"], &[b"PASS\n"]);
        let message = format!("{}: serial capture failed", super::BENIGN_QEMU_STOP_ERROR);
        let err =
            verify_qemu_success_contract(Err(anyhow::anyhow!(message.clone())), Some(&output))
                .unwrap_err();

        assert_eq!(err.to_string(), message);
    }

    #[test]
    fn benign_stop_without_success_contract_is_not_silently_accepted() {
        let message = super::BENIGN_QEMU_STOP_ERROR.to_string();
        let err =
            verify_qemu_success_contract(Err(anyhow::anyhow!(message.clone())), None).unwrap_err();

        assert_eq!(err.to_string(), message);
    }

    #[test]
    fn malformed_transcript_tail_suffix_is_not_ignored() {
        let output = captured_output(&["PASS"], &[b"PASS\n"]);
        let message = format!(
            "{}; transcript tail corrupted",
            super::BENIGN_QEMU_STOP_ERROR
        );
        let err =
            verify_qemu_success_contract(Err(anyhow::anyhow!(message.clone())), Some(&output))
                .unwrap_err();

        assert_eq!(err.to_string(), message);
    }

    #[test]
    fn benign_stop_with_newline_transcript_is_accepted() {
        let output = captured_output(&["PASS"], &[b"PASS\n"]);
        let message = format!(
            "{}; transcript tail:\nQEMU exited after success",
            super::BENIGN_QEMU_STOP_ERROR
        );

        verify_qemu_success_contract(Err(anyhow::anyhow!(message)), Some(&output)).unwrap();
    }
    #[test]
    fn unrelated_runner_error_still_wins_over_success_marker() {
        let output = captured_output(&["PASS"], &[b"PASS\n"]);
        let err = verify_qemu_success_contract(Err(anyhow::anyhow!("QEMU timeout")), Some(&output))
            .unwrap_err();

        assert_eq!(err.to_string(), "QEMU timeout");
    }

    #[test]
    fn runner_error_takes_precedence_over_missing_marker() {
        let output = captured_output(&["PASS"], &[]);
        let err = verify_qemu_success_contract(Err(anyhow::anyhow!("QEMU timeout")), Some(&output))
            .unwrap_err();

        assert_eq!(err.to_string(), "QEMU timeout");
    }

    #[test]
    fn success_marker_after_boot_log_and_across_chunks_is_accepted() {
        let output = captured_output(
            &[r"(?m)^guest smp ipi pass!\s*$"],
            &[b"booting\n~ # \nguest smp ", b"ipi pass!\n"],
        );

        verify_qemu_success_contract(
            Err(anyhow::anyhow!(
                "QEMU stopped without matching a configured success regex"
            )),
            Some(&output),
        )
        .unwrap();
    }

    #[test]
    fn streaming_matcher_preserves_chunk_boundaries() {
        let output = captured_output(
            &[r"(?m)^STARRY_GROUPED_TESTS_PASSED$"],
            &[b"STARRY_GROUPED_", b"TESTS_PASSED\n"],
        );
        verify_qemu_success_contract(Ok(()), Some(&output)).unwrap();
    }

    #[test]
    fn success_regex_ignores_ansi_csi_before_marker() {
        let patterns = vec![r"(?m)^guest smp ipi pass!\s*$".to_string()];
        let output = QemuSuccessOutput::new(&patterns);

        output.append(b"booting\n");
        output.append(b"\x1b[27;5Rguest smp ipi pass!\n");

        verify_qemu_success_contract(Ok(()), Some(&output)).unwrap();
    }

    #[test]
    fn success_regex_recovers_after_non_utf8_serial_noise() {
        let patterns = vec![r"(?m)^guest smp ipi pass!\s*$".to_string()];
        let output = QemuSuccessOutput::new(&patterns);

        output.append(b"booting\xff\xfeearly serial output\n");
        output.append(b"guest smp ipi pass!\n");

        verify_qemu_success_contract(Ok(()), Some(&output)).unwrap();
    }

    #[test]
    fn long_span_success_regex_preserves_streaming_state() {
        let patterns = vec![
            r"(?s)SG2002_DWC2_MSC_READ_PERF size=262144 .*SG2002_DWC2_BUSYWAIT_CHECK transfer_busy_wait_iters=0 .*SG2002_DWC2_MSC_BENCH_SUMMARY .*AXTEST_SUITE_OK"
                .to_string(),
        ];
        let output = QemuSuccessOutput::new(&patterns);
        let noise = vec![b'x'; TRANSCRIPT_TAIL_BYTES + 1];

        output.append(b"SG2002_DWC2_MSC_READ_PERF size=262144 ");
        output.append(&noise);
        output.append(b"SG2002_DWC2_BUSYWAIT_CHECK transfer_busy_wait_iters=0 ");
        output.append(&noise);
        output.append(b"SG2002_DWC2_MSC_BENCH_SUMMARY ");
        output.append(&noise);
        output.append(b"AXTEST_SUITE_OK\n");

        verify_qemu_success_contract(Ok(()), Some(&output)).unwrap();
    }

    #[test]
    fn sustained_noise_keeps_only_the_bounded_diagnostic_tail() {
        let output = captured_output(&["PASS"], &[]);
        let chunk = [b'x'; 8192];

        for _ in 0..1024 {
            output.append(&chunk);
        }

        let snapshot = output.snapshot();
        assert_eq!(snapshot.tail.len(), TRANSCRIPT_TAIL_BYTES);
        assert!(!snapshot.matched);
    }

    #[test]
    fn runner_error_takes_precedence_over_invalid_success_regex() {
        let output = captured_output(&["("], &[]);

        let err = verify_qemu_success_contract(Err(anyhow::anyhow!("QEMU timeout")), Some(&output))
            .unwrap_err();

        assert_eq!(err.to_string(), "QEMU timeout");
    }

    #[test]
    fn success_only_output_does_not_enable_backtrace_block_capture() {
        let output = captured_output(&["PASS"], &[]);
        let capture = crate::backtrace::BacktraceQemuCapture::success_output_only(output);

        assert!(!capture.captures_backtrace_blocks());
    }
}
