use std::{
    net::{SocketAddr, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
pub(crate) struct HostHttpProbeOutcome {
    pub(crate) output: Vec<u8>,
    pub(crate) verdict: anyhow::Result<()>,
}

impl HostHttpProbeOutcome {
    pub(crate) fn failed(error: anyhow::Error) -> Self {
        Self {
            output: Vec::new(),
            verdict: Err(error),
        }
    }
}

pub(super) fn wait_for_port_ready_until(
    addr: &str,
    connect_timeout: Duration,
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(connect_timeout)
        .context("HTTP readiness deadline overflow")?
        .min(deadline);
    let addr: SocketAddr = addr.parse()?;
    loop {
        if stop.load(Ordering::Acquire) {
            bail!("host http probe stopped");
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("HTTP readiness exhausted the member deadline");
        }
        if TcpStream::connect_timeout(&addr, remaining.min(Duration::from_millis(100))).is_ok() {
            return Ok(());
        }
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_cannot_renew_an_expired_member_budget_even_for_a_listening_port() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let stop = AtomicBool::new(false);
        let result = wait_for_port_ready_until(
            &listener.local_addr().unwrap().to_string(),
            Duration::from_secs(120),
            Instant::now() - Duration::from_millis(1),
            &stop,
        );
        assert!(result.is_err());
        assert!(!stop.load(Ordering::Acquire));
    }
}
