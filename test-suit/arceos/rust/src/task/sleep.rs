use std::{
    thread,
    time::{Duration, Instant},
};

pub fn run() -> crate::TestResult {
    let requested = Duration::from_millis(100);
    let tolerance = Duration::from_millis(5);
    let started = Instant::now();
    thread::sleep(requested);
    assert!(
        started.elapsed() >= requested - tolerance,
        "std sleep returned before its requested duration"
    );
    Ok(())
}
