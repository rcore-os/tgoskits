//! Stop-and-wait ACK/retry helpers and heartbeat / dedup state.

use core::time::Duration;

use crate::message::MessageType;

/// Default stop-and-wait timing (see `plans/技术方案.md` §3.2.3).
pub const DEFAULT_INITIAL_TIMEOUT_MS: u32 = 50;
pub const DEFAULT_MAX_TIMEOUT_MS: u32 = 2000;
pub const DEFAULT_MAX_RETRIES: u8 = 5;

/// Heartbeat: 1s interval, 3 consecutive misses => down.
pub const DEFAULT_HEARTBEAT_INTERVAL_MS: u32 = 1000;
pub const DEFAULT_HEARTBEAT_MISS_THRESHOLD: u8 = 3;

/// Dedup ring size for inbound seq tracking.
pub const DEDUP_WINDOW: usize = 32;

/// Stop-and-wait retry configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopWaitConfig {
    pub initial_timeout_ms: u32,
    pub max_timeout_ms: u32,
    pub max_retries: u8,
}

impl Default for StopWaitConfig {
    fn default() -> Self {
        Self {
            initial_timeout_ms: DEFAULT_INITIAL_TIMEOUT_MS,
            max_timeout_ms: DEFAULT_MAX_TIMEOUT_MS,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }
}

/// Outbound stop-and-wait session counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopWaitState {
    pub next_seq: u32,
    pub total_retries: u32,
    config: StopWaitConfig,
}

impl StopWaitState {
    pub const fn new(config: StopWaitConfig) -> Self {
        Self {
            next_seq: 1,
            total_retries: 0,
            config,
        }
    }

    pub fn alloc_seq(&mut self) -> u32 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        seq
    }

    pub fn record_retry(&mut self) {
        self.total_retries = self.total_retries.saturating_add(1);
    }

    /// Exponential backoff capped at [`StopWaitConfig::max_timeout_ms`].
    pub fn backoff_ms(&self, attempt: u8) -> u32 {
        let shift = attempt.min(6);
        let scaled = self.config.initial_timeout_ms.saturating_mul(1u32 << shift);
        scaled.min(self.config.max_timeout_ms)
    }

    pub fn max_retries(&self) -> u8 {
        self.config.max_retries
    }
}

/// Heartbeat liveness tracker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeartbeatConfig {
    pub interval_ms: u32,
    pub miss_threshold: u8,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval_ms: DEFAULT_HEARTBEAT_INTERVAL_MS,
            miss_threshold: DEFAULT_HEARTBEAT_MISS_THRESHOLD,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeartbeatState {
    misses: u8,
    config: HeartbeatConfig,
    link: LinkState,
}

impl HeartbeatState {
    pub const fn new(config: HeartbeatConfig) -> Self {
        Self {
            misses: 0,
            config,
            link: LinkState::Up,
        }
    }

    pub fn link_state(&self) -> LinkState {
        self.link
    }

    pub fn interval(&self) -> Duration {
        Duration::from_millis(u64::from(self.config.interval_ms))
    }

    pub fn on_response(&mut self) {
        self.misses = 0;
        self.link = LinkState::Up;
    }

    pub fn on_miss(&mut self) {
        self.misses = self.misses.saturating_add(1);
        if self.misses >= self.config.miss_threshold {
            self.link = LinkState::Down;
        }
    }
}

/// Fixed-size seq dedup ring (newest overwrites oldest slot on overflow).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DedupWindow {
    slots: [u32; DEDUP_WINDOW],
    head: usize,
}

impl DedupWindow {
    pub const fn new() -> Self {
        Self {
            slots: [0; DEDUP_WINDOW],
            head: 0,
        }
    }

    /// Returns `true` if `seq` was already seen.
    pub fn is_duplicate(&self, seq: u32) -> bool {
        self.slots.iter().any(|&s| s == seq)
    }

    pub fn remember(&mut self, seq: u32) {
        self.slots[self.head] = seq;
        self.head = (self.head + 1) % DEDUP_WINDOW;
    }
}

/// Expected response type for a stop-and-wait exchange.
pub fn ack_type_for_request(request: MessageType) -> Option<MessageType> {
    match request {
        MessageType::CtrlCmd => Some(MessageType::StateReport),
        MessageType::ErrorNotify => Some(MessageType::Ack),
        MessageType::Heartbeat => Some(MessageType::Heartbeat),
        MessageType::StateReport | MessageType::Ack => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_exponential_cap() {
        let st = StopWaitState::new(StopWaitConfig::default());
        assert_eq!(st.backoff_ms(0), 50);
        assert_eq!(st.backoff_ms(1), 100);
        assert_eq!(st.backoff_ms(2), 200);
        assert_eq!(st.backoff_ms(10), 2000);
    }

    #[test]
    fn dedup_window_tracks_seq() {
        let mut w = DedupWindow::new();
        assert!(!w.is_duplicate(42));
        w.remember(42);
        assert!(w.is_duplicate(42));
    }

    #[test]
    fn heartbeat_misses_mark_down() {
        let mut hb = HeartbeatState::new(HeartbeatConfig {
            interval_ms: 1000,
            miss_threshold: 3,
        });
        hb.on_miss();
        hb.on_miss();
        assert_eq!(hb.link_state(), LinkState::Up);
        hb.on_miss();
        assert_eq!(hb.link_state(), LinkState::Down);
        hb.on_response();
        assert_eq!(hb.link_state(), LinkState::Up);
    }

    #[test]
    fn ack_type_mapping() {
        assert_eq!(
            ack_type_for_request(MessageType::CtrlCmd),
            Some(MessageType::StateReport)
        );
        assert_eq!(
            ack_type_for_request(MessageType::ErrorNotify),
            Some(MessageType::Ack)
        );
    }
}
